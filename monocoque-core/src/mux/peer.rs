//! Per-peer state and the reader and writer loops that move its bytes.
//!
//! A wire peer owns one persistent reader loop and one persistent writer loop,
//! each boxed once when the peer comes up. The reader keeps a single owned read
//! buffer, decodes complete messages straight into the peer's ready queue, and
//! stops reading while that queue is at the receive high-water mark, so a slow
//! consumer backpressures its own peer through the kernel and nothing else.
//! The writer drains the peer's [`OutQueue`] one job at a time.
//!
//! Neither loop is ever cancelled by a timeout, so no in-flight read or write is
//! dropped and no received bytes are lost; a receive timeout only bounds how
//! long the user waits for the ready queue.

use super::out::{OutQueue, WriteJob};
use super::{CloseReason, Codec, EventQueue, PeerId};
use crate::msg::Msg;
use crate::transport::Stream;
use bytes::{Buf, Bytes, BytesMut};
use compio_buf::IoVectoredBuf;
use compio_buf::{BufResult, IntoInner, IoBuf};
use compio_io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

/// Fewest bytes a peer must take during one stall window to count as reading.
///
/// It also has to take this many to stop counting as stalled, so a peer
/// reading a trickle does not cost each send to it a whole window.
pub const WRITE_PROGRESS_FLOOR: u64 = 16 * 1024;

/// Largest a read grows to while a peer keeps filling it. Stays under the
/// allocator's mmap threshold (128 KiB in glibc), past which each new buffer
/// would cost page faults.
const MAX_READ_TARGET: usize = 64 * 1024;

/// Below this, a read that did not fill the buffer means the socket is drained.
const SMALL_READ: usize = 1024;

/// On tokio and smol, while reads keep filling the buffer (a stream with data
/// waiting), each read is given a full `read_size` even if that means a fresh
/// buffer, rather than whatever tail the last one left. Into the tail, 16 KiB
/// frames straddled every read and took two reads each, and coalesced 16 KiB
/// throughput trailed 0.4 by about 20% on both. Request-reply reads come back
/// short, so they keep reusing the tail and allocate nothing per message.
/// compio reads several large frames per read instead and keeps the tail.
const STREAM_FULL_READS: bool = cfg!(not(feature = "runtime-compio"));

/// Whether a read buffer with `spare` room left should be refreshed to the
/// read target before the next read.
///
/// The room left shrinks as frames are split off the front. Waiting until it
/// was nearly gone let reads drift down to a few hundred bytes: two-frame
/// messages, whose first frame keeps the buffer shared, fell to about 830
/// bytes a read, and on io_uring, where every read is a submission, to a sixth
/// of single-frame throughput. Refreshing once under a quarter of a read keeps
/// reads large, at one allocation per few dozen KiB when the buffer is still
/// shared. `filled` says the last read filled its buffer (see
/// [`STREAM_FULL_READS`]).
const fn needs_refresh(spare: usize, read_size: usize, filled: bool) -> bool {
    let floor = if read_size / 4 > MIN_READ {
        read_size / 4
    } else {
        MIN_READ
    };
    spare < floor || (STREAM_FULL_READS && filled && spare < read_size)
}

/// Smallest spare capacity a read is issued with.
const MIN_READ: usize = 512;

/// Reads a reader task makes in a row before it yields to the other tasks on
/// its thread. A yield costs one wake and one poll, noise against 16 reads.
const READS_PER_YIELD: u32 = 16;

/// Return Pending once, waking immediately, so other tasks get a turn.
async fn yield_now() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

/// What a peer's sends go through.
pub enum Outbound<C> {
    /// A byte stream with a protocol codec.
    Wire {
        /// Framing and transform state.
        codec: C,
        /// Encoded bytes waiting for the writer.
        out: OutQueue,
        /// The writer loop, parked while `out` is empty.
        writer_waker: Option<Waker>,
        /// With `write_coalescing`, the byte threshold below which writes are
        /// held. Held writes are released early by a flush or by reaching
        /// the send HWM, so a sender blocked on the HWM always makes progress.
        hold: Option<usize>,
        /// A flush asked for everything queued to be written.
        flushing: bool,
    },
    /// An inproc peer on this thread: messages move with no codec.
    Local(crate::localq::LocalSender<Msg>),
    /// An inproc peer on another thread.
    Remote(super::inproc::RemoteTx),
}

/// Mutable state of one peer, shared by its loops and the socket core.
pub struct PeerState<C> {
    pub(crate) id: PeerId,
    /// Complete inbound messages not yet received.
    pub(crate) ready: VecDeque<Msg>,
    pub(crate) recv_hwm: usize,
    pub(crate) send_hwm: usize,
    /// Keep only the newest inbound message (`ZMQ_CONFLATE`).
    pub(crate) conflate: bool,
    reader_waker: Option<Waker>,
    pub(crate) outbound: Outbound<C>,
    readable_signaled: bool,
    pub(crate) closed: Option<CloseReason>,
    pub(crate) events: Rc<EventQueue>,
    /// Last heartbeat tick that found bytes had arrived; tracked only when
    /// heartbeating. Reads only raise `recv_seen`, so the hot path never
    /// reads the clock.
    pub(crate) last_recv: Option<Instant>,
    pub(crate) recv_seen: bool,
    pub(crate) ping_outstanding: bool,
    /// Close the connection once everything queued has been written.
    pub(crate) close_when_drained: bool,
    /// Bytes the kernel has taken, so a waiting sender can tell a slow peer
    /// from one that stopped reading. A job's partial writes count.
    pub(crate) bytes_written: u64,
    /// A sender gave up waiting on this peer's writes. Eager sends stop
    /// waiting on it until it takes [`WRITE_PROGRESS_FLOOR`] more bytes or
    /// its queue drains.
    pub(crate) write_stalled: bool,
    /// `bytes_written` when the peer was marked stalled.
    stalled_at: u64,
    /// Progress and the time it was read, since sends began finding this
    /// peer's writes unfinished. Kept on the peer, across sends: a wait cut
    /// short by a `send_timeout` shorter than `write_stall` never saw a whole
    /// window by itself, so a peer that stopped reading was never marked.
    stall_watch: Option<(i128, Instant)>,
    /// The wire stream, to ask how much of what was written the peer has
    /// taken. Weak: the reader and writer alone keep the socket open.
    pub(crate) stream: Weak<Stream>,
}

impl<C: Codec> PeerState<C> {
    pub(crate) fn new(
        id: PeerId,
        outbound: Outbound<C>,
        recv_hwm: usize,
        send_hwm: usize,
        events: Rc<EventQueue>,
        heartbeat: bool,
    ) -> Self {
        Self {
            id,
            ready: VecDeque::new(),
            recv_hwm,
            send_hwm,
            conflate: false,
            reader_waker: None,
            outbound,
            readable_signaled: false,
            closed: None,
            events,
            last_recv: heartbeat.then(Instant::now),
            recv_seen: false,
            ping_outstanding: false,
            close_when_drained: false,
            bytes_written: 0,
            write_stalled: false,
            stalled_at: 0,
            stall_watch: None,
            stream: Weak::new(),
        }
    }

    /// Close once drained: wake the writer so it notices.
    pub(crate) fn close_after_drain(&mut self) {
        self.close_when_drained = true;
        match &mut self.outbound {
            Outbound::Wire { writer_waker, .. } => {
                if let Some(w) = writer_waker.take() {
                    w.wake();
                }
            }
            _ => self.close(CloseReason::Local),
        }
    }

    /// A conflating peer never fills: each message replaces the last.
    fn ready_full(&self) -> bool {
        !self.conflate && self.recv_hwm != 0 && self.ready.len() >= self.recv_hwm
    }

    #[inline]
    fn push_ready(&mut self, msg: Msg) {
        if self.conflate {
            self.ready.clear();
        }
        self.ready.push_back(msg);
    }

    /// Reader side: wait while the ready queue is at the receive HWM.
    fn poll_reader_room(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.ready_full() {
            self.reader_waker = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    /// Queue an inbound message and tell the core once per batch.
    pub(crate) fn deliver(&mut self, msg: Msg) {
        self.push_ready(msg);
        self.signal_readable();
    }

    fn signal_readable(&mut self) {
        if !self.readable_signaled {
            self.readable_signaled = true;
            self.events.push(super::Event::Readable(self.id));
        }
    }

    /// Take the next ready message, waking the reader if that made room.
    #[inline]
    pub(crate) fn pop_ready(&mut self) -> Option<Msg> {
        let msg = self.ready.pop_front()?;
        if let Some(w) = self.reader_waker.take() {
            w.wake();
        }
        Some(msg)
    }

    /// The core has seen the readable signal and queued this peer.
    pub(crate) const fn readable_seen(&mut self) {
        self.readable_signaled = false;
    }

    /// Messages queued toward this peer and not yet written.
    pub(crate) fn pending_out(&self) -> usize {
        match &self.outbound {
            Outbound::Wire { out, .. } => out.pending_msgs(),
            Outbound::Local(tx) => tx.len(),
            Outbound::Remote(tx) => tx.len(),
        }
    }

    /// `true` while bytes this peer will write on its own are not yet in the
    /// kernel. Held (coalescing) writes and inproc queues never count: the
    /// first waits for an explicit flush, the second for the receiver. Nor
    /// does a peer marked stalled, which no sender should wait for.
    pub(crate) fn write_pending(&self) -> bool {
        !self.write_stalled
            && matches!(&self.outbound, Outbound::Wire { out, hold: None, .. } if out.pending_msgs() > 0)
    }

    /// `true` if the writer may take queued bytes now.
    fn released(&self) -> bool {
        match &self.outbound {
            Outbound::Wire {
                out,
                hold: Some(threshold),
                flushing,
                ..
            } => {
                *flushing
                    || out.queued_bytes() >= *threshold
                    || (self.send_hwm != 0 && out.pending_msgs() >= self.send_hwm)
            }
            _ => true,
        }
    }

    fn wake_writer_if_released(&mut self) {
        if self.released()
            && let Outbound::Wire { writer_waker, .. } = &mut self.outbound
            && let Some(w) = writer_waker.take()
        {
            w.wake();
        }
    }

    /// `true` if one more message fits under the send HWM.
    pub(crate) fn writable(&self) -> bool {
        if self.closed.is_some() {
            return false;
        }
        match &self.outbound {
            Outbound::Wire { .. } => self.send_hwm == 0 || self.pending_out() < self.send_hwm,
            Outbound::Local(tx) => {
                tx.is_connected() && (self.send_hwm == 0 || tx.len() < self.send_hwm)
            }
            Outbound::Remote(tx) => {
                !tx.is_disconnected() && (self.send_hwm == 0 || tx.len() < self.send_hwm)
            }
        }
    }

    /// For a full inproc peer, have its receiver wake `cx` when it takes a
    /// message. Returns `true` if the peer has room after all (its receiver
    /// may run on another thread), so the caller retries instead of parking.
    pub(crate) fn watch_send_room(&self, cx: &mut Context<'_>) -> bool {
        if self.writable() {
            return true;
        }
        match &self.outbound {
            Outbound::Local(tx) => {
                let _ = tx.poll_ready(cx);
            }
            Outbound::Remote(tx) => tx.watch_room(cx, self.send_hwm),
            Outbound::Wire { .. } => {}
        }
        self.writable()
    }

    /// Queue `msg` toward this peer. The caller has checked [`writable`].
    ///
    /// [`writable`]: Self::writable
    pub(crate) fn enqueue(&mut self, msg: &Msg) -> crate::error::Result<()> {
        match &mut self.outbound {
            Outbound::Wire { codec, out, .. } => {
                codec.encode(msg, out)?;
                self.finish_encoded();
                Ok(())
            }
            Outbound::Local(tx) => tx
                .try_send(msg.clone())
                .map_err(|_| crate::error::MonocoqueError::Again),
            Outbound::Remote(tx) => {
                if tx.try_send(msg.clone()) {
                    Ok(())
                } else {
                    Err(crate::error::MonocoqueError::Again)
                }
            }
        }
    }

    /// Queue `msg` by value, avoiding the clone an inproc peer would need.
    pub(crate) fn enqueue_owned(&mut self, msg: Msg) -> crate::error::Result<()> {
        self.enqueue_prefixed(&[], msg)
    }

    /// A message was just encoded onto the queue. An eager send with the writer
    /// idle goes straight to the kernel here, with no task woken or polled;
    /// only what the socket would not take is left for the writer.
    fn finish_encoded(&mut self) {
        if let Outbound::Wire {
            out, hold: None, ..
        } = &mut self.outbound
            && let Some(tail) = out.sole_message()
            && let Some(stream) = self.stream.upgrade()
            && let Some(n) = stream.send_direct(tail)
        {
            self.bytes_written = self.bytes_written.wrapping_add(n as u64);
            if n == tail.len() {
                tail.clear();
                return;
            }
            tail.advance(n);
        }
        if let Outbound::Wire { out, .. } = &mut self.outbound {
            out.note_message();
        }
        self.wake_writer_if_released();
    }

    /// Queue `msg` toward a wire peer with room, encoding from the reference.
    /// `None` if the peer has no room or is not a wire peer; otherwise whether
    /// the peer is eager, the only kind whose write a sender waits for.
    #[inline]
    pub(crate) fn enqueue_ref(&mut self, msg: &Msg) -> Option<crate::error::Result<bool>> {
        if !self.writable() {
            return None;
        }
        let Outbound::Wire {
            codec, out, hold, ..
        } = &mut self.outbound
        else {
            return None;
        };
        let eager = hold.is_none();
        if let Err(e) = codec.encode(msg, out) {
            return Some(Err(e));
        }
        self.finish_encoded();
        Some(Ok(eager))
    }

    /// Queue the message in `slot`, leaving it empty. A wire peer encodes it
    /// where it is: a `Msg` is large, and moving it here costs a real copy.
    #[inline]
    pub(crate) fn enqueue_slot(
        &mut self,
        prefix: &[Bytes],
        slot: &mut Option<Msg>,
    ) -> crate::error::Result<()> {
        if let Outbound::Wire { codec, out, .. } = &mut self.outbound {
            let msg = slot.as_ref().expect("polled after completion");
            if prefix.is_empty() {
                codec.encode(msg, out)?;
            } else {
                codec.encode_prefixed(prefix, msg, out)?;
            }
            *slot = None;
            self.finish_encoded();
            return Ok(());
        }
        let msg = slot.take().expect("polled after completion");
        self.enqueue_prefixed(prefix, msg)
    }

    /// Queue `prefix` frames then `msg` as one message. A wire peer encodes
    /// the prefix in place; an inproc peer gets it prepended.
    pub(crate) fn enqueue_prefixed(
        &mut self,
        prefix: &[Bytes],
        mut msg: Msg,
    ) -> crate::error::Result<()> {
        if let Outbound::Wire { codec, out, .. } = &mut self.outbound {
            if prefix.is_empty() {
                codec.encode(&msg, out)?;
            } else {
                codec.encode_prefixed(prefix, &msg, out)?;
            }
            self.finish_encoded();
            return Ok(());
        }
        for frame in prefix.iter().rev() {
            msg.push_front(frame.clone());
        }
        match &mut self.outbound {
            Outbound::Local(tx) => tx
                .try_send(msg)
                .map_err(|_| crate::error::MonocoqueError::Again),
            Outbound::Remote(tx) => {
                if tx.try_send(msg) {
                    Ok(())
                } else {
                    Err(crate::error::MonocoqueError::Again)
                }
            }
            Outbound::Wire { .. } => self.enqueue(&msg),
        }
    }

    /// Queue already-encoded wire bytes (a fan-out shares one encoding).
    pub(crate) fn enqueue_wire(&mut self, wire: Bytes) {
        if let Outbound::Wire {
            out, writer_waker, ..
        } = &mut self.outbound
        {
            out.push_segment(wire);
            out.note_message();
            if let Some(w) = writer_waker.take() {
                w.wake();
            }
        }
    }

    /// Queue a protocol command (a heartbeat PING) outside the message count.
    pub(crate) fn enqueue_command(&mut self, encode: impl FnOnce(&mut C, &mut OutQueue)) {
        if let Outbound::Wire {
            codec,
            out,
            writer_waker,
            ..
        } = &mut self.outbound
        {
            encode(codec, out);
            if let Some(w) = writer_waker.take() {
                w.wake();
            }
        }
    }

    /// Release writes held for coalescing.
    pub(crate) fn flush(&mut self) {
        if let Outbound::Wire {
            out,
            writer_waker,
            flushing,
            ..
        } = &mut self.outbound
            && out.pending_msgs() > 0
        {
            *flushing = true;
            if let Some(w) = writer_waker.take() {
                w.wake();
            }
        }
    }

    /// Set the coalescing threshold; `None` writes eagerly.
    pub(crate) fn set_hold(&mut self, threshold: Option<usize>) {
        if let Outbound::Wire { hold, .. } = &mut self.outbound {
            *hold = threshold;
        }
        self.wake_writer_if_released();
    }

    /// `true` once everything queued has been written.
    pub(crate) fn drained(&self) -> bool {
        match &self.outbound {
            Outbound::Wire { out, .. } => out.pending_msgs() == 0,
            Outbound::Local(tx) => tx.is_empty() || !tx.is_connected(),
            Outbound::Remote(tx) => tx.is_empty() || tx.is_disconnected(),
        }
    }

    /// Arrange for `cx` to be woken as an inproc peer drains what was queued
    /// toward it. A wire peer's writer wakes the socket itself.
    pub(crate) fn watch_drained(&self, cx: &mut Context<'_>) {
        match &self.outbound {
            Outbound::Local(tx) => {
                let _ = tx.poll_empty(cx);
            }
            Outbound::Remote(tx) => tx.watch_drained(cx),
            Outbound::Wire { .. } => {}
        }
    }

    /// Writer side: take the next job, or park until there is one.
    ///
    /// `segment` is the payload of one TCP segment on this connection. A
    /// coalescing peer released by its threshold writes whole segments and
    /// keeps the rest for the next batch. A batch is rarely a whole number of
    /// segments, and the kernel sends the odd bytes at its end as a segment of
    /// their own, which costs the sender a second pass through the stack for
    /// a few hundred bytes: a 64 KiB threshold on loopback (65 483 bytes a
    /// segment) wrote 65 720, and writing 65 483 instead moved 256 B frames
    /// from 15.1 to 15.9 M msg/s. A flush, a close and a full queue write
    /// everything, as does a batch under one segment.
    fn poll_job(
        &mut self,
        cx: &mut Context<'_>,
        spare: &mut BytesMut,
        iov: &mut Vec<Bytes>,
        stream: &Stream,
    ) -> Poll<Option<WriteJob>> {
        if self.closed.is_some() {
            return Poll::Ready(None);
        }
        let released = self.released();
        let (hwm, closing) = (self.send_hwm, self.close_when_drained);
        let Outbound::Wire {
            out,
            writer_waker,
            hold,
            flushing,
            ..
        } = &mut self.outbound
        else {
            return Poll::Ready(None);
        };
        let (cap, segment) = if hold.is_some() {
            // Asked for each batch: the kernel holds a young connection to
            // half its window (32 KiB on loopback) and raises it later.
            let whole =
                released && !*flushing && !closing && (hwm == 0 || out.pending_msgs() < hwm);
            let segment = if whole { stream.segment_size() } else { 0 };
            (super::out::MAX_COALESCED_VECTORED_JOB, segment)
        } else {
            (super::out::MAX_VECTORED_JOB, 0)
        };
        if released && let Some(job) = out.take_job_capped(spare, iov, cap, segment) {
            return Poll::Ready(Some(job));
        }
        if self.close_when_drained {
            return Poll::Ready(None);
        }
        *writer_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    fn complete_job(&mut self) {
        if let Outbound::Wire { out, flushing, .. } = &mut self.outbound {
            out.complete_job();
            if out.pending_msgs() == 0 {
                *flushing = false;
                self.write_stalled = false;
                self.stall_watch = None;
            }
        }
        self.events.push(super::Event::Writable(self.id));
    }

    /// The kernel took `n` more bytes.
    const fn note_written(&mut self, n: usize) {
        self.bytes_written = self.bytes_written.wrapping_add(n as u64);
        if self.write_stalled
            && self.bytes_written.wrapping_sub(self.stalled_at) >= WRITE_PROGRESS_FLOOR
        {
            self.write_stalled = false;
        }
    }

    /// A figure that rises as the peer takes bytes: written minus what its
    /// kernel has not acknowledged. Only differences mean anything. A full
    /// socket buffer wakes a writer only once much of it is free, so at a low
    /// rate `bytes_written` alone sits still while the peer reads. One syscall.
    pub(crate) fn write_progress(&self) -> i128 {
        let unacked = self.stream.upgrade().map_or(0, |s| s.unacked_bytes());
        i128::from(self.bytes_written) - i128::from(unacked)
    }

    /// A send found this peer's writes unfinished. Watch its progress across
    /// sends, and mark it stalled once a whole `window` has passed with it
    /// taking under [`WRITE_PROGRESS_FLOOR`]. A peer taking that much starts
    /// the watch over: it is reading, however it is paced. `true` if this
    /// observation marked the peer.
    pub(crate) fn observe_unfinished_write(&mut self, window: std::time::Duration) -> bool {
        let now = self.write_progress();
        let Some((from, since)) = self.stall_watch else {
            self.stall_watch = Some((now, Instant::now()));
            return false;
        };
        if now - from >= i128::from(WRITE_PROGRESS_FLOOR) {
            self.stall_watch = Some((now, Instant::now()));
            return false;
        }
        // Within a tenth of the window, for timer granularity: without a
        // timeout the watch starts after one slice of a window and is judged
        // after the next.
        if since.elapsed() < window.saturating_sub(window / 10) {
            return false;
        }
        self.stall_watch = None;
        self.mark_write_stalled()
    }

    /// Stop eager sends waiting on this peer. `true` if it was not stalled.
    pub(crate) const fn mark_write_stalled(&mut self) -> bool {
        let newly = !self.write_stalled;
        self.write_stalled = true;
        self.stalled_at = self.bytes_written;
        newly
    }

    /// Mark the peer gone and tell the core, once.
    pub(crate) fn close(&mut self, reason: CloseReason) {
        if self.closed.is_none() {
            self.closed = Some(reason.clone());
            if let Outbound::Wire { out, .. } = &mut self.outbound {
                out.clear();
            }
            self.events.push(super::Event::Closed(self.id, reason));
        }
    }
}

/// How much to read next, after `n` bytes arrived with `spare` of room.
///
/// A read that filled the buffer left more behind: read more at once next time,
/// fewer syscalls per byte. A peer gone quiet brings it back down, so only a
/// busy connection holds more. Only a read given the whole target says anything:
/// one into the leftover tail of a buffer fills it or falls short by chance.
const fn next_read_target(target: usize, floor: usize, spare: usize, n: usize) -> usize {
    if spare < target {
        return target;
    }
    let cap = if MAX_READ_TARGET > floor {
        MAX_READ_TARGET
    } else {
        floor
    };
    if n == spare && target * 2 <= cap {
        target * 2
    } else if n == spare {
        cap
    } else if n < target / 4 && target / 2 >= floor {
        target / 2
    } else if n < target / 4 {
        floor
    } else {
        target
    }
}

/// Read, decode and queue messages from a wire peer until it closes.
pub async fn reader_loop<C: Codec>(
    peer: Rc<RefCell<PeerState<C>>>,
    stream: Rc<Stream>,
    read_size: usize,
) {
    let read_size = read_size.max(MIN_READ);
    let mut buf = BytesMut::new();
    {
        let mut st = peer.borrow_mut();
        let initial = match &mut st.outbound {
            Outbound::Wire { codec, .. } => codec.initial_message(),
            _ => None,
        };
        if let Some(msg) = initial {
            st.push_ready(msg);
            st.signal_readable();
        }
    }
    let (mut reads, mut target) = (0u32, read_size);
    let mut retired = super::readbuf::Retired::default();
    let mut short_read = false;
    let mut filled = false;
    loop {
        poll_fn(|cx| peer.borrow_mut().poll_reader_room(cx)).await;
        if peer.borrow().closed.is_some() {
            return;
        }
        // A readiness runtime with no task budget (smol) completes reads
        // without ever returning Pending while the peer keeps sending, and
        // with an unbounded receive HWM nothing else stops this loop, so it
        // would hold the thread. Step aside now and then; a ring read already
        // yields on every submission.
        reads = reads.wrapping_add(1);
        if reads.is_multiple_of(READS_PER_YIELD) {
            yield_now().await;
        }
        // A unique buffer is reclaimed from its front, and one still held by
        // undelivered messages is swapped for a kept one, so the steady state
        // allocates nothing. A frame longer than the buffer has already asked
        // for its full size.
        if needs_refresh(buf.capacity() - buf.len(), read_size, filled)
            && !matches!(&peer.borrow().outbound, Outbound::Wire { codec, .. } if codec.filling_frame())
        {
            retired.make_room(&mut buf, target);
        }
        if short_read {
            // Nothing more is coming for now: keep one buffer, not several.
            retired.clear();
            stream.wait_after_short_read().await;
        }
        let len = buf.len();
        let spare = buf.capacity() - len;
        let mut s = &*stream;
        let BufResult(res, slice) = s.read(buf.slice(len..)).await;
        buf = slice.into_inner();
        let mut st = peer.borrow_mut();
        match res {
            Ok(0) => {
                st.close(CloseReason::Eof);
                return;
            }
            Err(e) => {
                st.close(CloseReason::Io(e.kind()));
                return;
            }
            Ok(n) => {
                // A small read that fell short is a request-reply exchange,
                // with nothing more coming until this side answers. A larger
                // one is a stream, whose next bytes may already be here.
                short_read = n < spare && n < SMALL_READ;
                filled = n == spare;
                target = next_read_target(target, read_size, spare, n);
            }
        }
        st.recv_seen = true;
        st.ping_outstanding = false;
        let st = &mut *st;
        let Outbound::Wire {
            codec,
            out,
            writer_waker,
            ..
        } = &mut st.outbound
        else {
            return;
        };
        let replies_before = out.queued_bytes();
        let mut delivered = false;
        let conflate = st.conflate;
        let result = codec.decode(
            &mut buf,
            &mut |msg| {
                if conflate {
                    st.ready.clear();
                }
                st.ready.push_back(msg);
                delivered = true;
            },
            out,
        );
        if out.queued_bytes() != replies_before
            && let Some(w) = writer_waker.take()
        {
            w.wake();
        }
        if delivered {
            st.signal_readable();
        }
        if let Err(e) = result {
            st.close(CloseReason::Protocol(e.to_string()));
            return;
        }
        if buf.is_empty() {
            buf.clear();
        }
    }
}

/// The peer's stream, counting what each write gets into the kernel. Under
/// compio's `write_all` loops, so a job's partial writes count as they happen.
struct CountedWriter<'a, C> {
    stream: &'a Stream,
    peer: &'a RefCell<PeerState<C>>,
}

impl<C: Codec> CountedWriter<'_, C> {
    fn count<B>(&self, r: BufResult<usize, B>) -> BufResult<usize, B> {
        if let Ok(n) = r.0 {
            self.peer.borrow_mut().note_written(n);
        }
        r
    }
}

impl<C: Codec> AsyncWrite for CountedWriter<'_, C> {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        let mut s = self.stream;
        let r = s.write(buf).await;
        self.count(r)
    }

    async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        let mut s = self.stream;
        let r = s.write_vectored(buf).await;
        self.count(r)
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        let mut s = self.stream;
        s.flush().await
    }

    async fn shutdown(&mut self) -> std::io::Result<()> {
        self.stream.shutdown().await
    }
}

/// Write a wire peer's queued bytes until it closes.
pub async fn writer_loop<C: Codec>(peer: Rc<RefCell<PeerState<C>>>, stream: Rc<Stream>) {
    let mut spare = BytesMut::new();
    let mut iov: Vec<Bytes> = Vec::new();
    loop {
        let job = poll_fn(|cx| {
            peer.borrow_mut()
                .poll_job(cx, &mut spare, &mut iov, &stream)
        })
        .await;
        let Some(job) = job else {
            if peer.borrow().close_when_drained && peer.borrow().closed.is_none() {
                let _ = stream.shutdown().await;
                peer.borrow_mut().close(CloseReason::Local);
            }
            return;
        };
        let mut s = CountedWriter {
            stream: &stream,
            peer: &peer,
        };
        let res = match job {
            WriteJob::Contiguous(buf) => {
                let BufResult(res, mut buf) = s.write_all(buf).await;
                buf.clear();
                spare = buf;
                res
            }
            WriteJob::Vectored(v) => {
                let BufResult(res, mut v) = s.write_vectored_all(v).await;
                v.clear();
                iov = v;
                res
            }
        };
        let mut st = peer.borrow_mut();
        if let Err(e) = res {
            st.close(CloseReason::Io(e.kind()));
            return;
        }
        st.complete_job();
    }
}

/// Move messages from an inproc peer on this thread into the ready queue.
pub async fn local_reader_loop<C: Codec>(
    peer: Rc<RefCell<PeerState<C>>>,
    rx: crate::localq::LocalReceiver<Msg>,
) {
    loop {
        poll_fn(|cx| peer.borrow_mut().poll_reader_room(cx)).await;
        if peer.borrow().closed.is_some() {
            return;
        }
        if let Some(msg) = rx.recv().await {
            peer.borrow_mut().deliver(msg);
        } else {
            peer.borrow_mut().close(CloseReason::Eof);
            return;
        }
    }
}

/// Move messages from an inproc peer on another thread into the ready queue.
pub async fn remote_reader_loop<C: Codec>(
    peer: Rc<RefCell<PeerState<C>>>,
    rx: super::inproc::RemoteRx,
) {
    loop {
        poll_fn(|cx| peer.borrow_mut().poll_reader_room(cx)).await;
        if peer.borrow().closed.is_some() {
            return;
        }
        if let Some(msg) = rx.recv().await {
            peer.borrow_mut().deliver(msg);
        } else {
            peer.borrow_mut().close(CloseReason::Eof);
            return;
        }
    }
}
