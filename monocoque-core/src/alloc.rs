//! Allocation primitives for Monocoque
//!
//! This module is the ONLY place where unsafe memory manipulation is allowed.
//! All invariants are enforced here so the rest of the system can remain 100% safe.

#![allow(unsafe_code)]

use bytes::Bytes;
use compio_buf::{IoBufMut, SetBufInit};
use std::alloc::{Layout, alloc, dealloc};
use std::ptr::NonNull;
use std::sync::Arc;

/// Size of one slab page.
/// Tuned for cache locality and amortized allocation cost.
pub const PAGE_SIZE: usize = 64 * 1024;

/// Cache-line alignment to avoid false sharing.
pub const PAGE_ALIGN: usize = 128;

/// A slab page: a pinned chunk of memory.
///
/// Invariant:
/// - Memory is allocated once and never moved.
/// - Freed only when the last `Arc<Page>` is dropped.
struct Page {
    ptr: NonNull<u8>,
    /// Actual allocation size in bytes. Equal to `PAGE_SIZE` for normal pages;
    /// larger when a single request exceeds `PAGE_SIZE` (dedicated oversized page).
    size: usize,
}

unsafe impl Send for Page {}
unsafe impl Sync for Page {}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe {
            let layout = Layout::from_size_align_unchecked(self.size, PAGE_ALIGN);
            dealloc(self.ptr.as_ptr(), layout);
        }
    }
}

/// Owner passed into `Bytes::from_owner`
///
/// This guarantees:
/// - The backing slab page stays alive as long as any Bytes exists.
/// - No aliasing mutable access occurs after freeze.
struct PageOwner {
    page: Arc<Page>,
}

impl AsRef<[u8]> for PageOwner {
    fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.page.ptr.as_ptr(), self.page.size) }
    }
}

/// Mutable slab slice used ONLY during IO.
///
/// This type:
/// - Implements `IoBufMut` so compio can safely DMA into it.
/// - Is never exposed to user code.
/// - Is frozen into immutable `Bytes` after IO completes.
pub struct SlabMut {
    page: Arc<Page>,
    ptr: NonNull<u8>,
    cap: usize,
    len: usize,
}

unsafe impl Send for SlabMut {}
unsafe impl Sync for SlabMut {}

// SAFETY: SlabMut upholds IoBuf invariants:
// - as_buf_ptr() returns a valid pointer to initialized memory region
// - The memory region [ptr, ptr + len) contains initialized data
// - The buffer is pinned and stable during IO operations
unsafe impl compio_buf::IoBuf for SlabMut {
    #[inline]
    fn as_buf_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }

    #[inline]
    fn buf_len(&self) -> usize {
        self.len
    }

    #[inline]
    fn buf_capacity(&self) -> usize {
        self.cap
    }
}

// SAFETY: SlabMut upholds IoBufMut invariants:
// - as_buf_mut_ptr() returns a valid pointer to a writable memory region
// - The memory region [ptr, ptr + cap) is exclusively owned
// - The buffer is pinned and stable during IO operations
// - set_buf_init correctly updates the initialized length
unsafe impl IoBufMut for SlabMut {
    #[inline]
    fn as_buf_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl SetBufInit for SlabMut {
    #[inline]
    unsafe fn set_buf_init(&mut self, len: usize) {
        debug_assert!(len <= self.cap);
        self.len = len;
    }
}

impl SlabMut {
    /// Freeze this slab into immutable `Bytes`.
    ///
    /// # Safety invariants enforced here:
    /// - `ptr` is inside `page`
    /// - `len <= cap`
    /// - After this call, no mutable access exists
    #[must_use]
    pub fn freeze(self) -> Bytes {
        let base = self.page.ptr.as_ptr();
        let offset = unsafe { self.ptr.as_ptr().offset_from(base) } as usize;

        debug_assert!(offset + self.len <= self.page.size);

        let owner = PageOwner { page: self.page };

        // Create a Bytes covering the whole page, then slice.
        let full = Bytes::from_owner(owner);
        full.slice(offset..offset + self.len)
    }
}

/// Arena used by the IO thread.
///
/// Not thread-safe by design.
/// One arena per socket actor.
pub struct IoArena {
    current: Option<Arc<Page>>,
    /// Usable size of `current` in bytes.
    current_page_size: usize,
    offset: usize,
}

impl Default for IoArena {
    fn default() -> Self {
        Self::new()
    }
}

impl IoArena {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: None,
            current_page_size: PAGE_SIZE,
            offset: PAGE_SIZE, // force alloc on first use
        }
    }

    /// Allocate a mutable buffer suitable for a single IO read.
    ///
    /// For `size <= PAGE_SIZE` multiple allocations share the same slab page
    /// (bump-pointer, zero-overhead). For `size > PAGE_SIZE` a dedicated page
    /// of exactly `size` bytes is allocated; subsequent small allocations start
    /// a fresh normal page. No artificial ceiling on `size`.
    ///
    /// This guarantees:
    /// - Stable memory address
    /// - No reallocation
    /// - No aliasing with other `SlabMut`
    pub fn alloc_mut(&mut self, size: usize) -> SlabMut {
        // For requests that exceed a normal page, allocate a dedicated page of
        // the exact required size. This never applies in the common case
        // (read_buffer_size <= PAGE_SIZE), so the branch is always-not-taken
        // on the hot path and costs nothing in practice.
        let page_size = size.max(PAGE_SIZE);

        if self.current.is_none() || self.offset + size > self.current_page_size {
            self.alloc_page(page_size);
        }

        let page = self.current.as_ref().unwrap().clone();

        let ptr = unsafe { NonNull::new_unchecked(page.ptr.as_ptr().add(self.offset)) };

        self.offset += size;

        SlabMut {
            page,
            ptr,
            cap: size,
            len: 0,
        }
    }

    #[inline(never)]
    fn alloc_page(&mut self, page_size: usize) {
        unsafe {
            let layout = Layout::from_size_align_unchecked(page_size, PAGE_ALIGN);
            let ptr = alloc(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }

            self.current = Some(Arc::new(Page {
                ptr: NonNull::new_unchecked(ptr),
                size: page_size,
            }));
            self.current_page_size = page_size;
            self.offset = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compio_buf::IoBuf;

    #[test]
    fn oversized_alloc_does_not_panic_or_corrupt() {
        // Regression test: read_buffer_size > PAGE_SIZE used to cause a
        // buffer overflow (io_uring writes past the page) and a panic in
        // freeze() with "range end out of bounds".
        let mut arena = IoArena::new();
        let size = PAGE_SIZE + 1024;
        let mut slab = arena.alloc_mut(size);
        assert_eq!(slab.buf_capacity(), size);
        // Simulate io_uring reporting `size` bytes written.
        unsafe { slab.set_buf_init(size) };
        let bytes = slab.freeze();
        assert_eq!(bytes.len(), size);
    }

    #[test]
    fn normal_allocs_still_share_a_page() {
        // Two small allocations must come from the same backing page
        // (bump-pointer, no extra syscall).
        let mut arena = IoArena::new();
        let slab1 = arena.alloc_mut(4096);
        let slab2 = arena.alloc_mut(4096);
        assert!(std::ptr::eq(
            Arc::as_ptr(&slab1.page),
            Arc::as_ptr(&slab2.page),
        ));
    }

    #[test]
    fn oversized_alloc_followed_by_normal_gets_fresh_page() {
        // After a dedicated oversized page, the next normal allocation must
        // not share that oversized page (offset would overflow it).
        let mut arena = IoArena::new();
        let large = arena.alloc_mut(PAGE_SIZE + 512);
        let small = arena.alloc_mut(512);
        assert!(!std::ptr::eq(
            Arc::as_ptr(&large.page),
            Arc::as_ptr(&small.page),
        ));
    }
}
