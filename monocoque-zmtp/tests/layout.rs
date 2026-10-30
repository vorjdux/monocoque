//! Layout gate: the hot types keep their size.
//!
//! An instruction counter cannot see layout. A configuration field added to a
//! decoder, or a message type that grows, pushes hot fields across a cache
//! line without changing a single instruction; the same mistake elsewhere
//! cost 14% of multipart throughput. This test pins the sizes of the types
//! every message passes through, and `benches/instr_hotpath.rs` limits the
//! estimated cycles (which weigh simulated cache misses) alongside the
//! instruction count.
//!
//! A size here is allowed to change. When one fails, check what grew and why,
//! benchmark the hot path, and update the number in the same commit with the
//! reason.

use monocoque_core::msg::{Msg, MsgRef};
use monocoque_core::mux::OutQueue;
use monocoque_zmtp::wire::ZmtpCodec;
use std::mem::size_of;

/// Every message moves through these; growth costs cache lines per message.
/// The sizes are those of a 64-bit target.
#[test]
#[cfg(target_pointer_width = "64")]
fn message_types_keep_their_size() {
    // 64 bytes of inline payload plus the frame bookkeeping.
    assert_eq!(size_of::<Msg>(), 144, "Msg changed size");
    assert!(
        size_of::<MsgRef<'_>>() <= 2 * size_of::<usize>(),
        "MsgRef is a borrow"
    );
}

/// The per-connection state the reader and writer touch on every frame.
#[test]
#[cfg(target_pointer_width = "64")]
fn connection_state_keeps_its_size() {
    // The codec copies the two options it reads per frame (max frame size,
    // vectored threshold) instead of holding a `SocketOptions` (664 bytes),
    // so options can grow without moving the decoder's fields.
    assert_eq!(size_of::<ZmtpCodec>(), 280, "ZmtpCodec changed size");
    assert_eq!(size_of::<OutQueue>(), 80, "OutQueue changed size");
}
