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
/// - Preserving arena-allocated buffer segments through the pipeline
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
            let Some(mut front) = self.segs.pop_front() else {
                break;
            };
            if n >= front.len() {
                n -= front.len();
                continue;
            }
            // partially consumed
            front.advance(n);
            self.segs.push_front(front);
            break;
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
            let out = front.split_to(n);
            if front.is_empty() {
                self.segs.pop_front();
            }
            return Some(out);
        }

        // Spans segments: copy.
        let mut out = BytesMut::with_capacity(n);
        let mut remaining = n;
        while remaining > 0 {
            let seg = self.segs.pop_front().expect("len check ensures segments exist");
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
}
