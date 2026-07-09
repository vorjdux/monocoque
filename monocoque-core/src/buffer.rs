use bytes::{Buf, Bytes, BytesMut};
use std::collections::VecDeque;

/// A segmented buffer optimized for zero-copy operations.
///
/// This buffer holds multiple `Bytes` segments and provides efficient
/// extraction without unnecessary copies. When data is requested and
/// fits within a single segment, it can be returned with zero copies
/// (just a refcount increment on the underlying `Bytes`).
///
/// # Use Cases
///
/// - Protocol decoders reading frames from streaming data
/// - Minimizing memcpy overhead for small messages (< 8KB)
/// - Preserving zero-copy buffer segments through the pipeline
///
/// # Tradeoffs
///
/// - **Fast path**: Single-segment extraction is O(1) with no copy
/// - **Slow path**: Multi-segment extraction requires copying into contiguous buffer
/// - For large messages spanning many reads, the copy cost is unavoidable
#[derive(Debug, Default)]
pub struct SegmentedBuffer {
    segs: VecDeque<Bytes>,
    len: usize,
}

impl SegmentedBuffer {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            segs: VecDeque::new(),
            len: 0,
        }
    }

    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Return the first contiguous chunk without consuming it.
    #[inline]
    #[must_use]
    pub fn front_chunk(&self) -> &[u8] {
        self.segs.front().map_or(&[], Bytes::as_ref)
    }

    #[inline]
    pub fn push(&mut self, bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }
        self.len += bytes.len();
        self.segs.push_back(bytes);
    }

    /// Copy the first `n` bytes into `dst`.
    ///
    /// Returns `false` if fewer than `n` bytes are available.
    pub fn copy_prefix(&self, n: usize, dst: &mut [u8]) -> bool {
        if n > self.len || dst.len() < n {
            return false;
        }

        let mut remaining = n;
        let mut out_off = 0;
        for seg in &self.segs {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(seg.len());
            dst[out_off..out_off + take].copy_from_slice(&seg[..take]);
            out_off += take;
            remaining -= take;
        }
        true
    }

    /// Advance the queue by `n` bytes, dropping fully-consumed segments.
    ///
    /// # Panics
    ///
    /// Panics if `n > self.len`.
    pub fn advance(&mut self, mut n: usize) {
        assert!(n <= self.len);
        self.len -= n;

        while n > 0 {
            let Some(front) = self.segs.front_mut() else {
                break;
            };
            if n < front.len() {
                front.advance(n);
                break;
            }

            n -= front.len();
            self.segs.pop_front();
        }
    }

    /// Take exactly `n` bytes from the front of the queue.
    ///
    /// If the first segment contains all `n` bytes, this is zero-copy.
    /// If the bytes span multiple segments, this copies into a contiguous buffer.
    pub fn take_bytes(&mut self, n: usize) -> Option<Bytes> {
        if n == 0 {
            return Some(Bytes::new());
        }
        if n > self.len {
            return None;
        }

        let Some(front) = self.segs.front_mut() else {
            return None;
        };

        if front.len() >= n {
            self.len -= n;
            if front.len() == n {
                let out = self.segs.pop_front();
                return out;
            }

            let out = front.slice(..n);
            front.advance(n);
            if front.is_empty() {
                self.segs.pop_front();
            }
            return Some(out);
        }

        // Spans segments: copy.
        let mut out = BytesMut::with_capacity(n);
        let mut remaining = n;
        while remaining > 0 {
            let seg = self
                .segs
                .pop_front()
                .expect("len check ensures segments exist");
            let take = remaining.min(seg.len());
            out.extend_from_slice(&seg[..take]);
            remaining -= take;
            self.len -= take;
            if take < seg.len() {
                let mut rest = seg;
                rest.advance(take);
                self.segs.push_front(rest);
            }
        }

        Some(out.freeze())
    }

    /// Skip `skip` bytes, then take `n` bytes from the front of the queue.
    ///
    /// When the whole range sits in the first segment, this avoids the
    /// `advance(skip)` pop/push round trip before extracting the payload.
    #[inline]
    pub fn take_bytes_after(&mut self, skip: usize, n: usize) -> Option<Bytes> {
        let total = skip.checked_add(n)?;
        if total > self.len {
            return None;
        }
        Some(self.take_bytes_after_available(skip, n))
    }

    /// Skip `skip` bytes, then take `n` bytes when the caller has checked availability.
    ///
    /// # Panics
    ///
    /// Panics if `skip + n` overflows or exceeds the buffered length.
    #[inline]
    pub fn take_bytes_after_available(&mut self, skip: usize, n: usize) -> Bytes {
        let total = skip.checked_add(n).expect("skip + n must not overflow");
        assert!(total <= self.len);
        if n == 0 {
            self.advance(skip);
            return Bytes::new();
        }

        let front = self.segs.front_mut().expect("non-empty buffer");
        if front.len() >= total {
            self.len -= total;
            front.advance(skip);
            let out = front.split_to(n);
            if front.is_empty() {
                self.segs.pop_front();
            }
            return out;
        }

        self.advance(skip);
        self.take_bytes(n)
            .expect("len check ensures requested bytes are available")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn take_bytes_after_slices_within_front_segment() {
        let mut buf = SegmentedBuffer::new();
        buf.push(Bytes::from_static(b"hhpayloadtail"));

        let payload = buf.take_bytes_after(2, 7).unwrap();

        assert_eq!(&payload[..], b"payload");
        assert_eq!(buf.len(), 4);
        assert_eq!(&buf.take_bytes(4).unwrap()[..], b"tail");
    }

    #[test]
    fn take_bytes_after_falls_back_across_segments() {
        let mut buf = SegmentedBuffer::new();
        buf.push(Bytes::from_static(b"hhpay"));
        buf.push(Bytes::from_static(b"loadtail"));

        let payload = buf.take_bytes_after(2, 7).unwrap();

        assert_eq!(&payload[..], b"payload");
        assert_eq!(buf.len(), 4);
        assert_eq!(&buf.take_bytes(4).unwrap()[..], b"tail");
    }

    #[test]
    fn take_bytes_slices_single_segment_without_losing_remainder() {
        let mut buf = SegmentedBuffer::new();
        buf.push(Bytes::from_static(b"abcdef"));

        let out = buf.take_bytes(2).unwrap();
        assert_eq!(&out[..], b"ab");
        assert_eq!(buf.len(), 4);

        let rest = buf.take_bytes(4).unwrap();
        assert_eq!(&rest[..], b"cdef");
        assert!(buf.is_empty());
    }

    #[test]
    fn take_bytes_removes_exact_single_segment() {
        let mut buf = SegmentedBuffer::new();
        buf.push(Bytes::from_static(b"abc"));

        let out = buf.take_bytes(3).unwrap();
        assert_eq!(&out[..], b"abc");
        assert!(buf.is_empty());
    }

    #[test]
    fn take_bytes_copies_across_segments_and_preserves_tail() {
        let mut buf = SegmentedBuffer::new();
        buf.push(Bytes::from_static(b"abc"));
        buf.push(Bytes::from_static(b"def"));

        let out = buf.take_bytes(4).unwrap();
        assert_eq!(&out[..], b"abcd");
        assert_eq!(buf.len(), 2);

        let tail = buf.take_bytes(2).unwrap();
        assert_eq!(&tail[..], b"ef");
        assert!(buf.is_empty());
    }
}
