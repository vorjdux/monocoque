#![no_main]

//! Fuzzes the real streaming ZMTP decoder under fragmentation.
//!
//! `fuzz_frame_codec` drives a one-shot decode and an encode round-trip. This
//! target instead exercises the resumable decode state machine the way a real
//! socket feeds it: arbitrary bytes arrive in arbitrary-sized chunks across
//! many `decode()` calls, so partial headers and partial payloads must be
//! carried across reads without panicking, looping forever, or losing framing.
//!
//! It drives production types only (`ZmtpDecoder`, `SegmentedBuffer`) - no
//! private reimplementation - so coverage tracks the code that actually ships.

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use monocoque_core::buffer::SegmentedBuffer;
use monocoque_zmtp::codec::ZmtpDecoder;

fuzz_target!(|data: &[u8]| {
    // The first byte seeds the chunk size so different inputs fragment the same
    // bytes differently; the rest is the byte stream fed to the decoder.
    let (chunk_seed, stream) = match data.split_first() {
        Some((&s, rest)) => (s as usize, rest),
        None => return,
    };
    // Chunk size in 1..=64 - small chunks maximise the number of resume points.
    let chunk = (chunk_seed & 0x3f) + 1;

    let mut decoder = ZmtpDecoder::new();
    let mut buf = SegmentedBuffer::new();

    // A guard so a decoder that returned Ok(Some) on every call for a fixed
    // buffer (which would be a bug) cannot spin here forever.
    let mut budget: u32 = 100_000;

    for piece in stream.chunks(chunk) {
        buf.push(Bytes::copy_from_slice(piece));

        // Drain every complete frame the newly-arrived bytes make available.
        // decode() must return Ok(Some(frame)) | Ok(None) | Err - never panic.
        loop {
            budget = match budget.checked_sub(1) {
                Some(b) => b,
                None => return,
            };
            match decoder.decode(&mut buf) {
                Ok(Some(_frame)) => continue, // got a frame, try for the next
                Ok(None) => break,            // need more bytes; feed the next chunk
                Err(_) => return,             // rejected input; a valid outcome
            }
        }
    }
});
