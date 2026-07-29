//! Owned-buffer I/O helpers shared by every runtime backend.
//!
//! The runtime adapters (`rt::tokio`, `rt::smol`, and the native `rt::compio`
//! path) all speak compio's owned-buffer read/write contract. Two pieces of
//! that contract are fiddly enough that duplicating them per backend invites
//! drift:
//!
//! - declaring how many bytes a read initialized in a buffer's spare capacity,
//!   which is the workspace's single owned-buffer `unsafe` operation
//!   ([`fill_read`]); and
//! - building borrowed `IoSlice`s over an owned vectored buffer for one
//!   `writev` ([`with_vectored_slices`]).
//!
//! Centralizing both here gives each backend one call site instead of its own
//! copy, and keeps the lone `set_buf_init` `unsafe` behind a single documented
//! contract rather than repeated in every adapter.
//!
//! Like `tcp`, this module encapsulates its `unsafe` behind a safe API, so it
//! opts back into `unsafe_code` that the crate otherwise denies.
#![allow(unsafe_code)]

use bytes::BytesMut;
use compio_buf::{BufResult, IoBufMut, IoVectoredBuf};
use smallvec::SmallVec;
use std::io::{self, IoSlice};
use std::mem::MaybeUninit;

/// Size of a read scratch slab.
///
/// Doubles as the ceiling `read_buffer_size` clamps to and the minimum capacity
/// [`take_read_buffer`] grows a stash to. Equal to the old arena page size, so
/// the per-read buffer bound and its resident footprint are unchanged.
pub const READ_SLAB_SIZE: usize = 64 * 1024;

/// Take a read-sized scratch buffer from the front of `stash`, leaving the
/// remaining tail in `stash` to hand out on the next call.
///
/// This replaces the old bump-pointer read arena: successive reads carve
/// `read_size` chunks off one `READ_SLAB_SIZE` allocation until it is used up,
/// then a fresh slab is allocated. Freezing a returned buffer shares that
/// slab's allocation (via `bytes` refcounting), so a lagging consumer pins the
/// slab exactly as the arena page did.
///
/// # Safety
///
/// The returned buffer reports `read_size` initialized bytes that are in fact
/// uninitialized, so it can be handed to an owned-buffer read without
/// zero-filling first. The caller must pass it straight to a read and
/// `truncate` it to the number of bytes actually read before freezing,
/// inspecting, or otherwise exposing its contents.
pub unsafe fn take_read_buffer(stash: &mut BytesMut, read_size: usize) -> BytesMut {
    if stash.capacity() < read_size {
        *stash = BytesMut::with_capacity(read_size.max(READ_SLAB_SIZE));
    }
    if stash.len() < read_size {
        // SAFETY: `read_size` is within capacity per the check above. The
        // function's contract requires the caller to overwrite these bytes via
        // the read and truncate to the real count before exposing them.
        unsafe { stash.set_len(read_size) };
    }
    let tail = stash.split_off(read_size);
    std::mem::replace(stash, tail)
}

/// Read into an owned buffer's spare capacity, then declare the bytes written
/// as initialized.
///
/// This is the one place in the workspace that calls `set_len` (from compio's
/// `SetLen`). Every runtime backend routes its read path through here, so the
/// single owned-buffer `unsafe` block lives behind one documented contract
/// instead of a copy per adapter.
///
/// `read` is an async closure handed the buffer's uninitialized spare capacity
/// as `&mut [MaybeUninit<u8>]`; it performs the actual read into the front of
/// that slice: tokio wraps it in a `ReadBuf`, smol hands it to `recv`, so the
/// read mechanism stays with the backend while the buffer bookkeeping stays
/// here. `AsyncFnOnce` lets the returned future borrow the spare slice, which a
/// plain `FnOnce` returning a future cannot.
///
/// # Contract
///
/// On `Ok(n)`, `read` must have initialized exactly the first `n` bytes of the
/// slice it was given (`n` never exceeds the slice length). `set_buf_init(n)`
/// then declares precisely those bytes live, matching what was written. A
/// backend that reported more bytes than it wrote, or wrote them anywhere but
/// the front of the slice, would break this contract and the `unsafe` below.
pub async fn fill_read<B, F>(mut buf: B, read: F) -> BufResult<usize, B>
where
    B: IoBufMut,
    F: AsyncFnOnce(&mut [MaybeUninit<u8>]) -> io::Result<usize>,
{
    // Scope the spare-capacity borrow to the read so `buf` is free to mutate
    // again once the count is known.
    let (outcome, spare_len) = {
        let spare = buf.as_uninit();
        let spare_len = spare.len();
        (read(spare).await, spare_len)
    };
    match outcome {
        Ok(n) => {
            // A backend that reports more bytes than the spare slice held would
            // make the set_len below declare uninitialized (or out-of-bounds)
            // memory live. Catch that contract violation in debug builds.
            debug_assert!(
                n <= spare_len,
                "read reported {n} bytes but the spare slice was only {spare_len}"
            );
            // SAFETY: per this function's contract, `read` initialized exactly
            // the first `n` bytes of the spare slice it was handed. Declaring
            // that same length initialized matches what was actually written.
            unsafe {
                buf.set_len(n);
            }
            BufResult(Ok(n), buf)
        }
        Err(e) => BufResult(Err(e), buf),
    }
}

/// Build borrowed `IoSlice`s over the initialized bytes of each buffer in an
/// owned vectored buffer and hand them to `f` for a single vectored write.
///
/// The slices borrow `buf`, so they stay valid only for the duration of `f`.
/// A `SmallVec` keeps the common case (a frame header plus a handful of frames)
/// off the heap; it spills to a `Vec` only past 16 buffers. Centralized here so
/// the smol `writev` path and the instruction-count bench share one builder.
pub fn with_vectored_slices<B, R>(buf: &B, f: impl FnOnce(&[IoSlice<'_>]) -> R) -> R
where
    B: IoVectoredBuf,
{
    let slices: SmallVec<[IoSlice<'_>; 16]> = buf.iter_slice().map(IoSlice::new).collect();
    f(&slices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_makes_slab_use_track_bytes_read_and_freezes_only_read_bytes() {
        // One 64 KiB slab, fully initialized so we can read pointers/contents.
        let mut stash = BytesMut::new();
        stash.resize(READ_SLAB_SIZE, 0);
        let slab_ptr = stash.as_ptr();

        // Carve a read buffer, "read" 10 bytes into it, and reclaim the unused
        // tail back into the slab (the read_raw hot path).
        let mut buf = unsafe { take_read_buffer(&mut stash, 8192) };
        assert_eq!(buf.as_ptr(), slab_ptr, "carved from the slab front");
        buf[..10].copy_from_slice(b"0123456789");
        buf.truncate(10);
        let mut reclaimed = buf.split_off(10);
        // Scratch len so unsplit does not discard the empty tail (see read_raw).
        // SAFETY: uninitialized scratch, overwritten before the next read.
        unsafe {
            reclaimed.set_len(reclaimed.capacity());
        }
        reclaimed.unsplit(std::mem::take(&mut stash));
        stash = reclaimed;

        // Freezing exposes exactly the 10 bytes read - never any of the
        // uninitialized carve.
        let frozen = buf.freeze();
        assert_eq!(frozen.as_ref(), b"0123456789");
        assert_eq!(frozen.as_ptr(), slab_ptr, "frozen frame aliases the slab head");

        // The reclaim returned the unused bytes: the next carve advances by the
        // 10 bytes read, not the full 8192 carve, and reuses the same slab.
        let next = unsafe { take_read_buffer(&mut stash, 8192) };
        assert_eq!(
            next.as_ptr() as usize,
            slab_ptr as usize + 10,
            "the next carve reuses the reclaimed space (slab use tracks bytes read)"
        );
    }

    #[test]
    fn take_read_buffer_over_slab_size_does_not_panic_or_corrupt() {
        // Regression (migrated from the old arena): a read_size larger than the
        // slab size once overflowed the backing page and panicked in freeze()
        // with "range end out of bounds". take_read_buffer must grow to fit.
        let mut stash = BytesMut::with_capacity(READ_SLAB_SIZE);
        let read_size = READ_SLAB_SIZE + 1024;
        // SAFETY: bookkeeping-only test; we truncate to the full length and
        // never rely on the (uninitialized) contents.
        let mut buf = unsafe { take_read_buffer(&mut stash, read_size) };
        assert_eq!(buf.len(), read_size);
        buf.truncate(read_size);
        assert_eq!(buf.freeze().len(), read_size);
    }

    #[test]
    fn take_read_buffer_splits_front_and_keeps_tail() {
        let mut stash = BytesMut::with_capacity(READ_SLAB_SIZE);
        // SAFETY: bookkeeping-only test; contents are never inspected.
        let buf = unsafe { take_read_buffer(&mut stash, 256) };
        assert_eq!(buf.len(), 256);
        assert_eq!(stash.len(), 0);
        assert!(stash.capacity() >= READ_SLAB_SIZE - 256);
    }

    #[test]
    fn take_read_buffer_reuses_one_slab_across_reads() {
        // Successive sub-slab reads carve from the same allocation, the way the
        // bump-pointer arena shared a page: the stash tail shrinks by exactly
        // read_size each time, with no fresh 64 KiB allocation.
        let mut stash = BytesMut::with_capacity(READ_SLAB_SIZE);
        // SAFETY: bookkeeping-only test; contents are never inspected.
        let _first = unsafe { take_read_buffer(&mut stash, 4096) };
        assert_eq!(stash.capacity(), READ_SLAB_SIZE - 4096);
        // SAFETY: bookkeeping-only test.
        let _second = unsafe { take_read_buffer(&mut stash, 4096) };
        assert_eq!(stash.capacity(), READ_SLAB_SIZE - 8192);
    }
}
