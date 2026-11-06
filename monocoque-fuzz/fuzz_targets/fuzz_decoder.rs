#![no_main]

//! Fuzzes the real streaming ZMTP decoder under fragmentation.
//!
//! `fuzz_frame_codec` drives a one-shot decode and an encode round-trip. This
//! target instead exercises the resumable decode state machine the way a real
//! connection feeds it: arbitrary bytes arrive in arbitrary-sized chunks across
//! many `decode()` calls, so partial headers and partial payloads must be
//! carried across reads without panicking, looping forever, or losing framing.
//!
//! It drives the production codec (`ZmtpCodec`, the one every socket uses), so
//! coverage tracks the code that actually ships.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use monocoque_core::mux::{Codec, OutQueue};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::wire::ZmtpCodec;

fuzz_target!(|data: &[u8]| {
    // The first byte seeds the chunk size so different inputs fragment the same
    // bytes differently; the rest is the byte stream fed to the decoder.
    let (chunk_seed, stream) = match data.split_first() {
        Some((&s, rest)) => (s as usize, rest),
        None => return,
    };
    // Chunk size in 1..=64 - small chunks maximise the number of resume points.
    let chunk = (chunk_seed & 0x3f) + 1;

    let mut codec = ZmtpCodec::new(None, &SocketOptions::default());
    let mut buf = BytesMut::new();
    let mut replies = OutQueue::default();

    for piece in stream.chunks(chunk) {
        buf.extend_from_slice(piece);
        // decode() consumes every complete frame and leaves a partial one in
        // place; it must return Ok or Err, never panic.
        if codec.decode(&mut buf, &mut |_msg| {}, &mut replies).is_err() {
            return;
        }
    }
});
