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

/// The largest read a connection issues at once: the ceiling
/// `read_buffer_size` clamps to.
pub const MAX_READ_SIZE: usize = 64 * 1024;

/// Read into an owned buffer, then declare the bytes written as initialized.
///
/// As with compio's own reads, the read fills the buffer from its start
/// (`IoBufMut::as_uninit` is the whole buffer); a caller that appends passes
/// a slice past what it holds, `buf.slice(len..)`.
///
/// This is the one place in the workspace that calls `set_len` (from compio's
/// `SetLen`). Every runtime backend routes its read path through here, so the
/// single owned-buffer `unsafe` block lives behind one documented contract
/// instead of a copy per adapter.
///
/// `read` is an async closure handed the buffer's storage as
/// `&mut [MaybeUninit<u8>]`; it performs the actual read into the front of
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
    use bytes::BytesMut;

    fn write_cdef(spare: &mut [MaybeUninit<u8>]) -> usize {
        for (slot, b) in spare.iter_mut().zip(b"cdef") {
            slot.write(*b);
        }
        4
    }

    /// `fill_read` declares exactly the bytes the read wrote. Runs under
    /// Miri, which checks that no uninitialized byte is ever exposed.
    #[test]
    fn fill_read_exposes_exactly_the_bytes_read() {
        let buf = BytesMut::with_capacity(32);
        let BufResult(res, buf) =
            futures::executor::block_on(fill_read(buf, async |s: &mut [MaybeUninit<u8>]| {
                Ok(write_cdef(s))
            }));
        assert_eq!(res.unwrap(), 4);
        assert_eq!(&buf[..], b"cdef");
    }

    /// Appending goes through a slice past what the buffer holds, as the
    /// socket reader does; the bytes already there survive.
    #[test]
    fn fill_read_through_a_slice_appends() {
        use compio_buf::{IntoInner, IoBuf};
        let mut buf = BytesMut::with_capacity(32);
        buf.extend_from_slice(b"ab");
        let len = buf.len();
        let BufResult(res, slice) = futures::executor::block_on(fill_read(
            buf.slice(len..),
            async |s: &mut [MaybeUninit<u8>]| Ok(write_cdef(s)),
        ));
        assert_eq!(res.unwrap(), 4);
        assert_eq!(&slice.into_inner()[..], b"abcdef");
    }

    #[test]
    fn fill_read_leaves_the_buffer_alone_on_error() {
        let mut buf = BytesMut::with_capacity(8);
        buf.extend_from_slice(b"x");
        let BufResult(res, buf) =
            futures::executor::block_on(fill_read(buf, async |_: &mut [MaybeUninit<u8>]| {
                Err(io::Error::other("no"))
            }));
        assert!(res.is_err());
        assert_eq!(&buf[..], b"x");
    }
}
