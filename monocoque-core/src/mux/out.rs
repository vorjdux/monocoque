//! A peer's outbound byte queue.
//!
//! Encoded bytes accumulate in a contiguous `tail` buffer. A frame the encoder
//! would rather not copy (a large body, or a message encoded once for a fan-out)
//! is pushed as its own `Bytes` segment after the tail written so far, so order
//! is preserved and the body reaches the kernel without a userspace copy.
//!
//! The writer takes everything queued as one job: the tail by value when there
//! are no segments (one `write`, its allocation returned afterwards), otherwise
//! the segments as one vectored write. While a job is in flight new sends keep
//! appending, so they go out together with the next job. That is the only
//! coalescing that happens by default: it recovers syscalls already lost to a
//! write in flight and never holds a message back.

use bytes::{Bytes, BytesMut};
use std::collections::VecDeque;

/// Most segments handed to one vectored write.
pub const MAX_IOV: usize = 64;

/// A vectored job stops taking segments once it holds this many bytes.
///
/// A write the socket buffer cannot take whole leaves the writer asleep until much of
/// the buffer is free and the reader asleep once it has drained the rest, so
/// large frames go faster written a few at a time.
#[cfg(not(feature = "runtime-compio"))]
pub const MAX_VECTORED_JOB: usize = 32 * 1024;
#[cfg(feature = "runtime-compio")]
pub const MAX_VECTORED_JOB: usize = 128 * 1024;

/// [`MAX_VECTORED_JOB`] for a coalescing peer.
///
/// A coalescing peer writes in batches anyway, and 128 KiB jobs halved its
/// send calls on 16 KiB frames. An eager one on tokio or smol lost 14% at that
/// size, so it keeps the smaller job.
pub const MAX_COALESCED_VECTORED_JOB: usize = 128 * 1024;

/// A contiguous job is cut to whole segments only when what is left over is
/// at most this fraction of it (one part in this many).
///
/// The leftover is copied into the next buffer. After a threshold release it
/// is less than a message, a few hundred bytes. On io_uring, where the queue
/// grows while a write is in flight, jobs come in any size and the leftover
/// averaged half a segment: copying that cost more than the short segment,
/// 6.96 down to 6.05 M msg/s at 1 KiB.
const MAX_COPIED_REST: usize = 16;

/// Bytes waiting to be written to one peer.
#[derive(Default)]
pub struct OutQueue {
    segs: VecDeque<Bytes>,
    /// Total length of `segs`, kept as they are pushed and popped. Summing
    /// them was done on every coalesced send; with large frames, two segments
    /// each, queued toward the HWM that was a walk over some 2 000 segments
    /// per send.
    seg_bytes: usize,
    tail: BytesMut,
    queued_msgs: usize,
    in_flight_msgs: usize,
    /// A job is out with the writer. `in_flight_msgs` cannot say so: protocol
    /// commands (a PONG, a heartbeat PING) are not counted as messages, so a
    /// job of commands alone leaves it at zero while its bytes are still
    /// being written.
    job_in_flight: bool,
}

/// One unit of work for a peer's writer.
pub enum WriteJob {
    /// Contiguous bytes, written with one `write_all`.
    Contiguous(BytesMut),
    /// Several segments, written with one vectored write.
    Vectored(Vec<Bytes>),
}

impl OutQueue {
    /// The contiguous buffer an encoder appends small frames to.
    pub fn buf(&mut self) -> &mut BytesMut {
        &mut self.tail
    }

    /// Append a segment after everything queued so far, without copying it.
    pub fn push_segment(&mut self, seg: Bytes) {
        if seg.is_empty() {
            return;
        }
        self.seal_tail();
        self.seg_bytes += seg.len();
        self.segs.push_back(seg);
    }

    /// Move the tail's bytes, if any, behind the segments as one more.
    fn seal_tail(&mut self) {
        if !self.tail.is_empty() {
            self.seg_bytes += self.tail.len();
            self.segs.push_back(self.tail.split().freeze());
        }
    }

    /// The bytes of the one message just encoded, if it is all there is: no
    /// segment, nothing queued before it and no write in flight. Writing them
    /// while a job is out would let them overtake or split it on the wire.
    pub fn sole_message(&mut self) -> Option<&mut BytesMut> {
        (self.segs.is_empty() && self.queued_msgs == 0 && !self.job_in_flight)
            .then_some(&mut self.tail)
    }

    /// Count one more queued message toward the high-water mark.
    pub const fn note_message(&mut self) {
        self.queued_msgs += 1;
    }

    /// Messages queued or in flight.
    pub const fn pending_msgs(&self) -> usize {
        self.queued_msgs + self.in_flight_msgs
    }

    /// `true` when nothing is queued (a job may still be in flight).
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty() && self.tail.is_empty()
    }

    /// Bytes queued and not yet handed to the writer.
    pub fn queued_bytes(&self) -> usize {
        self.tail.len() + self.seg_bytes
    }

    /// Take everything queued as one job.
    ///
    /// `spare` is swapped in as the new tail so the steady state allocates
    /// nothing; `iov` is reused for the vectored case.
    pub fn take_job(&mut self, spare: &mut BytesMut, iov: &mut Vec<Bytes>) -> Option<WriteJob> {
        self.take_job_capped(spare, iov, MAX_VECTORED_JOB, 0)
    }

    /// [`take_job`](Self::take_job) with a vectored job held to `max_bytes`.
    ///
    /// With `segment` non-zero and at least that much queued, the job is cut
    /// to a whole number of `segment` bytes and the rest stays queued (see
    /// [`PeerState::poll_job`](super::peer::PeerState) for why).
    pub fn take_job_capped(
        &mut self,
        spare: &mut BytesMut,
        iov: &mut Vec<Bytes>,
        max_bytes: usize,
        segment: usize,
    ) -> Option<WriteJob> {
        if self.is_empty() {
            return None;
        }
        self.job_in_flight = true;
        if self.segs.is_empty() {
            let len = self.tail.len();
            // The rest is copied to the next buffer, which pays only while it
            // is small beside what the cut saves.
            let rest = match len.checked_rem(segment) {
                Some(rest) if len >= segment && rest <= len / MAX_COPIED_REST => rest,
                _ => 0,
            };
            spare.clear();
            spare.extend_from_slice(&self.tail[len - rest..]);
            self.tail.truncate(len - rest);
            self.take_queued_share(len - rest, len);
            let job = std::mem::replace(&mut self.tail, std::mem::take(spare));
            return Some(WriteJob::Contiguous(job));
        }
        self.seal_tail();
        iov.clear();
        let queued = self.seg_bytes;
        let mut bytes = 0;
        while iov.len() < MAX_IOV
            && bytes < max_bytes
            && let Some(seg) = self.segs.pop_front()
        {
            bytes += seg.len();
            iov.push(seg);
        }
        let took_all = self.segs.is_empty();
        let mut rest = if segment == 0 || bytes < segment {
            0
        } else {
            bytes % segment
        };
        bytes -= rest;
        while rest > 0
            && let Some(mut last) = iov.pop()
        {
            if last.len() > rest {
                let back = last.split_off(last.len() - rest);
                iov.push(last);
                self.segs.push_front(back);
                break;
            }
            rest -= last.len();
            self.segs.push_front(last);
        }
        self.seg_bytes -= bytes;
        // A job cut at MAX_IOV or at the byte cap leaves segments of queued
        // messages behind. Those messages stay counted as queued until the job
        // that takes their last segment, so the HWM and `drained` never see
        // them as written. One that took all there was and gave back a part
        // segment counts the share it wrote.
        if self.segs.is_empty() {
            self.take_queued_msgs();
        } else if took_all {
            self.take_queued_share(bytes, queued);
        }
        Some(WriteJob::Vectored(std::mem::take(iov)))
    }

    const fn take_queued_msgs(&mut self) {
        self.in_flight_msgs += self.queued_msgs;
        self.queued_msgs = 0;
    }

    /// Count as in flight the messages in `taken` of the `total` bytes that
    /// were queued. Where a message ends in the bytes is not recorded, so
    /// this goes by proportion, and while bytes stay queued so does at least
    /// one message: `drained` must not report an empty queue with bytes in it.
    fn take_queued_share(&mut self, taken: usize, total: usize) {
        if taken == total {
            return self.take_queued_msgs();
        }
        let share = self.queued_msgs.saturating_mul(taken) / total.max(1);
        let moved = share.min(self.queued_msgs.saturating_sub(1));
        self.in_flight_msgs += moved;
        self.queued_msgs -= moved;
    }

    /// The job handed out by [`take_job`](Self::take_job) has completed.
    pub const fn complete_job(&mut self) {
        self.in_flight_msgs = 0;
        self.job_in_flight = false;
    }

    /// Discard everything queued (the peer is gone, or linger is zero).
    pub fn clear(&mut self) {
        self.segs.clear();
        self.seg_bytes = 0;
        self.tail.clear();
        self.queued_msgs = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(job: WriteJob) -> Vec<u8> {
        match job {
            WriteJob::Contiguous(b) => b.to_vec(),
            WriteJob::Vectored(v) => v.iter().flat_map(|b| b.iter().copied()).collect(),
        }
    }

    #[test]
    fn small_writes_coalesce_into_one_contiguous_job() {
        let mut q = OutQueue::default();
        q.buf().extend_from_slice(b"ab");
        q.note_message();
        q.buf().extend_from_slice(b"cd");
        q.note_message();
        assert_eq!(q.pending_msgs(), 2);
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q.take_job(&mut spare, &mut iov).unwrap();
        assert!(matches!(job, WriteJob::Contiguous(_)));
        assert_eq!(flatten(job), b"abcd");
        assert!(q.is_empty());
        assert_eq!(q.pending_msgs(), 2);
        q.complete_job();
        assert_eq!(q.pending_msgs(), 0);
    }

    /// A job of commands alone (PONGs, a heartbeat PING) counts no message,
    /// but while it is out a message must not go around it to the kernel: it
    /// would split the job's frames or overtake them.
    #[test]
    fn no_direct_write_while_a_command_only_job_is_out() {
        let mut q = OutQueue::default();
        q.buf().extend_from_slice(b"\x04\x05PONG!");
        assert!(q.sole_message().is_some(), "nothing is out yet");
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q.take_job(&mut spare, &mut iov).unwrap();
        assert_eq!(q.pending_msgs(), 0, "commands are not messages");
        q.buf().extend_from_slice(b"\x00\x01m");
        assert!(q.sole_message().is_none(), "the command job is still out");
        drop(job);
        q.complete_job();
        assert!(q.sole_message().is_some());
    }

    /// `queued_bytes` is a running count; it has to match the bytes actually
    /// queued through pushes, a job that takes only part, and a clear.
    #[test]
    fn queued_bytes_tracks_segments_and_tail() {
        let actual = |q: &OutQueue| q.tail.len() + q.segs.iter().map(Bytes::len).sum::<usize>();
        let mut q = OutQueue::default();
        for _ in 0..10 {
            q.buf().extend_from_slice(b"hdr");
            q.push_segment(Bytes::from_static(&[7u8; 100]));
            q.note_message();
        }
        q.buf().extend_from_slice(b"end");
        assert_eq!(q.queued_bytes(), 10 * 103 + 3);
        assert_eq!(q.queued_bytes(), actual(&q));
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q.take_job_capped(&mut spare, &mut iov, 250, 0).unwrap();
        let taken = flatten(job).len();
        assert!(taken < 10 * 103 + 3, "the cap left part of the queue");
        assert_eq!(q.queued_bytes(), 10 * 103 + 3 - taken);
        assert_eq!(q.queued_bytes(), actual(&q));
        q.complete_job();
        while let Some(job) = q.take_job(&mut spare, &mut iov) {
            drop(job);
            q.complete_job();
        }
        assert_eq!(q.queued_bytes(), 0);
        q.push_segment(Bytes::from_static(b"abc"));
        q.clear();
        assert_eq!(q.queued_bytes(), 0);
        assert_eq!(q.queued_bytes(), actual(&q));
    }

    /// A contiguous job cut to whole segments keeps the rest, in order, and
    /// keeps a message counted for it.
    #[test]
    fn a_contiguous_job_is_cut_to_whole_segments() {
        let mut q = OutQueue::default();
        for i in 0..10u8 {
            q.buf().extend_from_slice(&[i; 30]);
            q.note_message();
        }
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q
            .take_job_capped(&mut spare, &mut iov, usize::MAX, 145)
            .unwrap();
        let first = flatten(job);
        assert_eq!(first.len(), 290, "two whole segments of the 300 bytes");
        assert_eq!(q.queued_bytes(), 10);
        assert_eq!(q.pending_msgs(), 10);
        q.complete_job();
        assert!(q.pending_msgs() >= 1, "bytes are still queued");
        assert!(
            q.pending_msgs() <= 2,
            "{} of 10 still counted",
            q.pending_msgs()
        );
        // Less than a segment is left: the next job takes it all.
        let rest = flatten(
            q.take_job_capped(&mut spare, &mut iov, usize::MAX, 145)
                .unwrap(),
        );
        q.complete_job();
        assert_eq!(q.pending_msgs(), 0);
        let all: Vec<u8> = first.into_iter().chain(rest).collect();
        let sent: Vec<u8> = (0..10u8).flat_map(|i| [i; 30]).collect();
        assert_eq!(all, sent);
    }

    /// A leftover that is a large part of the job is not worth copying: the
    /// job goes out whole.
    #[test]
    fn a_contiguous_job_with_a_large_leftover_is_not_cut() {
        let mut q = OutQueue::default();
        q.buf().extend_from_slice(&[1; 300]);
        q.note_message();
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q
            .take_job_capped(&mut spare, &mut iov, usize::MAX, 128)
            .unwrap();
        assert_eq!(flatten(job).len(), 300, "44 of 300 bytes would be copied");
        assert!(q.is_empty());
    }

    /// The same for a vectored job, whose last segment is split without a copy.
    #[test]
    fn a_vectored_job_is_cut_to_whole_segments() {
        let mut q = OutQueue::default();
        let mut sent = Vec::new();
        for i in 0..5u8 {
            q.buf().extend_from_slice(&[i; 9]);
            q.push_segment(Bytes::from(vec![i + 100; 91]));
            q.note_message();
            sent.extend_from_slice(&[i; 9]);
            sent.extend_from_slice(&[i + 100; 91]);
        }
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q
            .take_job_capped(&mut spare, &mut iov, usize::MAX, 128)
            .unwrap();
        let mut all = flatten(job);
        assert_eq!(all.len(), 384, "three whole segments of the 500 bytes");
        assert_eq!(q.queued_bytes(), 116);
        q.complete_job();
        assert!(q.pending_msgs() >= 1 && q.pending_msgs() <= 2);
        all.extend(flatten(
            q.take_job_capped(&mut spare, &mut iov, usize::MAX, 128)
                .unwrap(),
        ));
        q.complete_job();
        assert_eq!(q.pending_msgs(), 0);
        assert_eq!(q.queued_bytes(), 0);
        assert_eq!(all, sent);
    }

    /// Under one segment there is nothing to cut: everything goes.
    #[test]
    fn less_than_a_segment_is_written_whole() {
        let mut q = OutQueue::default();
        q.buf().extend_from_slice(&[1; 100]);
        q.note_message();
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q
            .take_job_capped(&mut spare, &mut iov, usize::MAX, 128)
            .unwrap();
        assert_eq!(flatten(job).len(), 100);
        assert!(q.is_empty());
    }

    #[test]
    fn segments_keep_order_with_the_tail() {
        let mut q = OutQueue::default();
        q.buf().extend_from_slice(b"h1");
        q.push_segment(Bytes::from_static(b"BODY1"));
        q.buf().extend_from_slice(b"h2");
        q.push_segment(Bytes::from_static(b"BODY2"));
        q.buf().extend_from_slice(b"end");
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let job = q.take_job(&mut spare, &mut iov).unwrap();
        assert!(matches!(job, WriteJob::Vectored(_)));
        assert_eq!(flatten(job), b"h1BODY1h2BODY2end");
        assert!(q.is_empty());
    }

    /// A batch longer than MAX_IOV goes out over several jobs; until the last
    /// one completes its messages are still pending, or a sender at its HWM
    /// would be let through and `close` would drop the rest unwritten.
    #[test]
    fn messages_stay_pending_until_their_last_segment_is_written() {
        let mut q = OutQueue::default();
        let messages = MAX_IOV;
        for _ in 0..messages {
            q.buf().extend_from_slice(b"hdr");
            q.push_segment(Bytes::from_static(b"body"));
            q.note_message();
        }
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        let mut written = Vec::new();
        let first = q.take_job(&mut spare, &mut iov).unwrap();
        written.extend(flatten(first));
        q.complete_job();
        assert!(!q.is_empty(), "the batch should not fit one job");
        assert_eq!(q.pending_msgs(), messages, "written early");
        while let Some(job) = q.take_job(&mut spare, &mut iov) {
            written.extend(flatten(job));
            q.complete_job();
        }
        assert_eq!(q.pending_msgs(), 0);
        assert_eq!(written, b"hdrbody".repeat(messages));
    }

    #[test]
    fn spare_buffer_is_recycled_as_the_next_tail() {
        let mut q = OutQueue::default();
        q.buf().extend_from_slice(&[1; 100]);
        let mut spare = BytesMut::with_capacity(4096);
        let mut iov = Vec::new();
        let WriteJob::Contiguous(written) = q.take_job(&mut spare, &mut iov).unwrap() else {
            panic!("expected a contiguous job");
        };
        assert!(q.buf().capacity() >= 4096);
        assert_eq!(written.len(), 100);
    }
}
