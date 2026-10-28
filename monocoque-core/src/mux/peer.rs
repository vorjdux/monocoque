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
use bytes::{Bytes, BytesMut};
use compio_buf::{BufResult, IntoInner, IoBuf};
use compio_io::{AsyncRead, AsyncWriteExt};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Instant;

/// Smallest spare capacity a read is issued with.
const MIN_READ: usize = 512;

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
        /// Hold writes until an explicit flush (`write_coalescing`).
        hold: bool,
    },
    /// An inproc peer on this thread: messages move with no codec.
    Local(crate::localq::LocalSender<Msg>),
    /// An inproc peer on another thread.
    Remote(flume::Sender<Msg>),
}

/// Mutable state of one peer, shared by its loops and the socket core.
pub struct PeerState<C> {
    pub(crate) id: PeerId,
    /// Complete inbound messages not yet received.
    pub(crate) ready: VecDeque<Msg>,
    pub(crate) recv_hwm: usize,
    pub(crate) send_hwm: usize,
    reader_waker: Option<Waker>,
    pub(crate) outbound: Outbound<C>,
    readable_signaled: bool,
    pub(crate) closed: Option<CloseReason>,
    pub(crate) events: Rc<EventQueue>,
    /// Last time any bytes arrived; tracked only when heartbeating.
    pub(crate) last_recv: Option<Instant>,
    pub(crate) ping_outstanding: bool,
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
            reader_waker: None,
            outbound,
            readable_signaled: false,
            closed: None,
            events,
            last_recv: heartbeat.then(Instant::now),
            ping_outstanding: false,
        }
    }

    fn ready_full(&self) -> bool {
        self.recv_hwm != 0 && self.ready.len() >= self.recv_hwm
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
        self.ready.push_back(msg);
        self.signal_readable();
    }

    fn signal_readable(&mut self) {
        if !self.readable_signaled {
            self.readable_signaled = true;
            self.events.push(super::Event::Readable(self.id));
        }
    }

    /// Take the next ready message, waking the reader if that made room.
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

    /// Queue `msg` toward this peer. The caller has checked [`writable`].
    ///
    /// [`writable`]: Self::writable
    pub(crate) fn enqueue(&mut self, msg: &Msg) -> crate::error::Result<()> {
        match &mut self.outbound {
            Outbound::Wire {
                codec,
                out,
                writer_waker,
                hold,
            } => {
                codec.encode(msg, out)?;
                out.note_message();
                if !*hold && let Some(w) = writer_waker.take() {
                    w.wake();
                }
                Ok(())
            }
            Outbound::Local(tx) => tx
                .try_send(msg.clone())
                .map_err(|_| crate::error::MonocoqueError::Again),
            Outbound::Remote(tx) => tx
                .try_send(msg.clone())
                .map_err(|_| crate::error::MonocoqueError::Again),
        }
    }

    /// Queue `msg` by value, avoiding the clone an inproc peer would need.
    pub(crate) fn enqueue_owned(&mut self, msg: Msg) -> crate::error::Result<()> {
        self.enqueue_prefixed(&[], msg)
    }

    /// Queue `prefix` frames then `msg` as one message. A wire peer encodes
    /// the prefix in place; an inproc peer gets it prepended.
    pub(crate) fn enqueue_prefixed(
        &mut self,
        prefix: &[Bytes],
        mut msg: Msg,
    ) -> crate::error::Result<()> {
        if let Outbound::Wire {
            codec,
            out,
            writer_waker,
            hold,
        } = &mut self.outbound
        {
            if prefix.is_empty() {
                codec.encode(&msg, out)?;
            } else {
                codec.encode_prefixed(prefix, &msg, out)?;
            }
            out.note_message();
            if !*hold && let Some(w) = writer_waker.take() {
                w.wake();
            }
            return Ok(());
        }
        for frame in prefix.iter().rev() {
            msg.push_front(frame.clone());
        }
        match &mut self.outbound {
            Outbound::Local(tx) => tx
                .try_send(msg)
                .map_err(|_| crate::error::MonocoqueError::Again),
            Outbound::Remote(tx) => tx
                .try_send(msg)
                .map_err(|_| crate::error::MonocoqueError::Again),
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
            writer_waker, hold, ..
        } = &mut self.outbound
        {
            *hold = false;
            if let Some(w) = writer_waker.take() {
                w.wake();
            }
        }
    }

    /// Set whether writes are held until an explicit flush.
    pub(crate) fn set_hold(&mut self, hold_writes: bool) {
        if let Outbound::Wire { hold, .. } = &mut self.outbound {
            *hold = hold_writes;
        }
    }

    /// `true` once everything queued has been written.
    pub(crate) fn drained(&self) -> bool {
        match &self.outbound {
            Outbound::Wire { out, .. } => out.pending_msgs() == 0,
            Outbound::Local(tx) => tx.is_empty() || !tx.is_connected(),
            Outbound::Remote(tx) => tx.is_empty() || tx.is_disconnected(),
        }
    }

    /// Writer side: take the next job, or park until there is one.
    fn poll_job(
        &mut self,
        cx: &mut Context<'_>,
        spare: &mut BytesMut,
        iov: &mut Vec<Bytes>,
    ) -> Poll<Option<WriteJob>> {
        if self.closed.is_some() {
            return Poll::Ready(None);
        }
        let Outbound::Wire {
            out,
            writer_waker,
            hold,
            ..
        } = &mut self.outbound
        else {
            return Poll::Ready(None);
        };
        if !*hold && let Some(job) = out.take_job(spare, iov) {
            return Poll::Ready(Some(job));
        }
        *writer_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    fn complete_job(&mut self) {
        if let Outbound::Wire { out, .. } = &mut self.outbound {
            out.complete_job();
        }
        self.events.push(super::Event::Writable(self.id));
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

/// Read, decode and queue messages from a wire peer until it closes.
pub async fn reader_loop<C: Codec>(
    peer: Rc<RefCell<PeerState<C>>>,
    stream: Rc<Stream>,
    read_size: usize,
) {
    let read_size = read_size.max(MIN_READ);
    let mut buf = BytesMut::new();
    loop {
        poll_fn(|cx| peer.borrow_mut().poll_reader_room(cx)).await;
        if peer.borrow().closed.is_some() {
            return;
        }
        // `reserve` reclaims the front of a unique buffer instead of
        // reallocating, so the steady state reuses one allocation. A frame
        // longer than the buffer has already asked for its full size.
        if buf.capacity() - buf.len() < MIN_READ {
            buf.reserve(read_size);
        }
        let len = buf.len();
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
            Ok(_) => {}
        }
        if st.last_recv.is_some() {
            st.last_recv = Some(Instant::now());
            st.ping_outstanding = false;
        }
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
        let result = codec.decode(
            &mut buf,
            &mut |msg| {
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

/// Write a wire peer's queued bytes until it closes.
pub async fn writer_loop<C: Codec>(peer: Rc<RefCell<PeerState<C>>>, stream: Rc<Stream>) {
    let mut spare = BytesMut::new();
    let mut iov: Vec<Bytes> = Vec::new();
    loop {
        let job = poll_fn(|cx| peer.borrow_mut().poll_job(cx, &mut spare, &mut iov)).await;
        let Some(job) = job else {
            return;
        };
        let mut s = &*stream;
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
    rx: flume::Receiver<Msg>,
) {
    loop {
        poll_fn(|cx| peer.borrow_mut().poll_reader_room(cx)).await;
        if peer.borrow().closed.is_some() {
            return;
        }
        if let Ok(msg) = rx.recv_async().await {
            peer.borrow_mut().deliver(msg);
        } else {
            peer.borrow_mut().close(CloseReason::Eof);
            return;
        }
    }
}
