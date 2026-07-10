#![no_main]

//! Fuzz the ZMTP greeting parser.
//!
//! `ZmtpGreeting::parse` reads a fixed 64-byte greeting straight off the wire
//! during the handshake, so it is directly reachable from a remote peer before
//! any authentication. It must never panic on arbitrary input; only `Ok`/`Err`.

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use monocoque_zmtp::ZmtpGreeting;

fuzz_target!(|data: &[u8]| {
    // Parse arbitrary bytes as a greeting. The parser expects exactly 64 bytes
    // but must reject any other length without panicking.
    let _ = ZmtpGreeting::parse(&Bytes::copy_from_slice(data));

    // Also exercise the exact-64-byte path so the field decoding is covered even
    // when the raw input is a different length.
    if data.len() >= 64 {
        let mut buf = [0u8; 64];
        buf.copy_from_slice(&data[..64]);
        let _ = ZmtpGreeting::parse(&Bytes::copy_from_slice(&buf));
    }
});
