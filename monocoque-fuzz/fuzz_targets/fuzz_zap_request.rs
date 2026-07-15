#![no_main]

//! Fuzz the ZAP (ZeroMQ Authentication Protocol) request/response parsers.
//!
//! ZAP messages are multipart frames exchanged between the socket and an
//! authentication handler. `ZapRequest::decode` / `ZapResponse::decode` run on
//! frames whose count and contents are attacker-influenced, so they must never
//! panic on a wrong frame count or malformed field; only `Ok`/`Err`. A dedicated
//! target (with its own corpus) rather than relying on incidental coverage in
//! the PLAIN target.

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse};

/// Split `data` into up to `max` frames using the first byte as a count hint.
fn split_frames(data: &[u8], max: usize) -> Vec<Bytes> {
    if data.len() < 2 {
        return Vec::new();
    }
    let n = (data[0] as usize % max) + 1;
    let payload = &data[1..];
    let chunk = (payload.len() / n).max(1);
    payload
        .chunks(chunk)
        .take(n)
        .map(Bytes::copy_from_slice)
        .collect()
}

fuzz_target!(|data: &[u8]| {
    // Arbitrary frame counts/contents must not panic the decoders.
    let req_frames = split_frames(data, 9);
    let _ = ZapRequest::decode(&req_frames);

    let resp_frames = split_frames(data, 6);
    let _ = ZapResponse::decode(&resp_frames);
});
