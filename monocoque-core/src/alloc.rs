//! Allocation primitives for Monocoque
//!
//! This module is the ONLY place where unsafe memory manipulation is allowed.
//! All invariants are enforced here so the rest of the system can remain 100% safe.

#![allow(unsafe_code)]

use bytes::Bytes;
use compio::buf::{IoBufMut, SetBufInit};
use std::alloc::{alloc, dealloc, Layout};
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
/// - Freed only when the last Arc<Page> is dropped.
struct Page {
    ptr: NonNull<u8>,
}

unsafe impl Send for Page {}
unsafe impl Sync for Page {}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe {
            let layout = Layout::from_size_align_unchecked(PAGE_SIZE, PAGE_ALIGN);
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
        unsafe { std::slice::from_raw_parts(self.page.ptr.as_ptr(), PAGE_SIZE) }
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
unsafe impl compio::buf::IoBuf for SlabMut {
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

        debug_assert!(offset + self.len <= PAGE_SIZE);

        let owner = PageOwner { page: self.page };

        // Create a Bytes covering the whole page, then slice.
        let full = Bytes::from_owner(owner);
        full.slice(offset..offset + self.len)
    }
}

/// Zero-copy `IoBuf` wrapper for Bytes.
///
/// This enables passing Bytes directly to compio write operations
/// without the .`to_vec()` memcpy that violates blueprint zero-copy guarantees.
///
/// SAFETY: Bytes is immutable and refcounted, so it's safe to expose as `IoBuf`.
pub struct IoBytes(Bytes);

impl IoBytes {
    pub const fn new(bytes: Bytes) -> Self {
        Self(bytes)
    }
}

impl From<Bytes> for IoBytes {
    fn from(bytes: Bytes) -> Self {
        Self(bytes)
    }
}

impl From<IoBytes> for Bytes {
    fn from(io_bytes: IoBytes) -> Self {
        io_bytes.0
    }
}

// SAFETY: IoBytes wraps Bytes which is:
// - Immutable (no mutable aliasing)
// - Refcounted (memory stays valid)
// - Contiguous (valid pointer/len guarantees)
unsafe impl compio::buf::IoBuf for IoBytes {
    #[inline]
    fn as_buf_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }

    #[inline]
    fn buf_len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    fn buf_capacity(&self) -> usize {
        self.0.len()
    }
}

/// Arena used by the IO thread.
///
/// Not thread-safe by design.
/// One arena per socket actor.
pub struct IoArena {
    current: Option<Arc<Page>>,
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
            offset: PAGE_SIZE, // force alloc on first use
        }
    }

    /// Allocate a mutable buffer suitable for a single IO read.
    ///
    /// This guarantees:
    /// - Stable memory address
    /// - No reallocation
    /// - No aliasing with other `SlabMut`
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `size > PAGE_SIZE`.
    pub fn alloc_mut(&mut self, size: usize) -> SlabMut {
        debug_assert!(size <= PAGE_SIZE);

        if self.current.is_none() || self.offset + size > PAGE_SIZE {
            self.alloc_page();
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
    fn alloc_page(&mut self) {
        unsafe {
            let layout = Layout::from_size_align_unchecked(PAGE_SIZE, PAGE_ALIGN);
            let ptr = alloc(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }

            self.current = Some(Arc::new(Page {
                ptr: NonNull::new_unchecked(ptr),
            }));
            self.offset = 0;
        }
    }
}
