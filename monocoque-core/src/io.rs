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

use compio_buf::{BufResult, IoBufMut, IoVectoredBuf};
use smallvec::SmallVec;
use std::io::{self, IoSlice};
use std::mem::MaybeUninit;

/// Read into an owned buffer's spare capacity, then declare the bytes written
/// as initialized.
///
/// This is the one place in the workspace that calls
/// [`IoBufMut::set_buf_init`]. Every runtime backend routes its read path
/// through here, so the single owned-buffer `unsafe` block lives behind one
/// documented contract instead of a copy per adapter.
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
    let outcome = {
        let spare = buf.as_mut_slice();
        read(spare).await
    };
    match outcome {
        Ok(n) => {
            // SAFETY: per this function's contract, `read` initialized exactly
            // the first `n` bytes of the spare slice it was handed. Declaring
            // that same length initialized matches what was actually written.
            unsafe {
                buf.set_buf_init(n);
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
    let slices: SmallVec<[IoSlice<'_>; 16]> = buf
        .as_dyn_bufs()
        .map(|b| IoSlice::new(b.as_slice()))
        .collect();
    f(&slices)
}
