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

/// Bytes waiting to be written to one peer.
#[derive(Default)]
pub struct OutQueue {
    segs: VecDeque<Bytes>,
    tail: BytesMut,
    queued_msgs: usize,
    in_flight_msgs: usize,
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
        if !self.tail.is_empty() {
            self.segs.push_back(self.tail.split().freeze());
        }
        self.segs.push_back(seg);
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
        self.tail.len() + self.segs.iter().map(Bytes::len).sum::<usize>()
    }

    /// Take everything queued as one job.
    ///
    /// `spare` is swapped in as the new tail so the steady state allocates
    /// nothing; `iov` is reused for the vectored case.
    pub fn take_job(&mut self, spare: &mut BytesMut, iov: &mut Vec<Bytes>) -> Option<WriteJob> {
        if self.is_empty() {
            return None;
        }
        self.in_flight_msgs += self.queued_msgs;
        self.queued_msgs = 0;
        if self.segs.is_empty() {
            spare.clear();
            let job = std::mem::replace(&mut self.tail, std::mem::take(spare));
            return Some(WriteJob::Contiguous(job));
        }
        if !self.tail.is_empty() {
            self.segs.push_back(self.tail.split().freeze());
        }
        iov.clear();
        let n = self.segs.len().min(MAX_IOV);
        iov.extend(self.segs.drain(..n));
        Some(WriteJob::Vectored(std::mem::take(iov)))
    }

    /// The job handed out by [`take_job`](Self::take_job) has completed.
    pub const fn complete_job(&mut self) {
        // Messages straddling a MAX_IOV split are released when the job holding
        // their last segment completes; counting them on the first is an
        // approximation that only ever releases HWM room early by one job.
        self.in_flight_msgs = 0;
    }

    /// Discard everything queued (the peer is gone, or linger is zero).
    pub fn clear(&mut self) {
        self.segs.clear();
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
