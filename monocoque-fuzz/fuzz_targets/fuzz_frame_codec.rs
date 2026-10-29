#![no_main]

use bytes::{Bytes, BytesMut};
use libfuzzer_sys::fuzz_target;
use monocoque_core::msg::Msg;
use monocoque_core::mux::{Codec, OutQueue};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::wire::{ZmtpCodec, encode_plain};

fn codec() -> ZmtpCodec {
    ZmtpCodec::new(None, &SocketOptions::default())
}

fuzz_target!(|data: &[u8]| {
    // --- Decode path: feed arbitrary bytes and assert no panics ---
    let mut buf = BytesMut::from(data);
    let mut replies = OutQueue::default();
    let _ = codec().decode(&mut buf, &mut |_msg| {}, &mut replies);

    // --- Encode path: derive a frame size from the first 2 bytes and encode ---
    if data.len() >= 2 {
        let size = u16::from_le_bytes([data[0], data[1]]) as usize;

        // Build a payload of `size` bytes sourced from the fuzz input.
        let payload: Vec<u8> = data.iter().cycle().take(size).copied().collect();
        let msg = Msg::from(Bytes::from(payload));

        let mut encoded = BytesMut::new();
        encode_plain(&msg, &mut encoded);

        // The encoded output is a header (2 or 9 bytes) plus the payload.
        let header = if size >= 256 { 9 } else { 2 };
        assert_eq!(encoded.len(), header + size);

        // Round-trip: a well-formed encoded frame must decode to the same body.
        let mut decoded = Vec::new();
        codec()
            .decode(&mut encoded, &mut |m| decoded.push(m), &mut replies)
            .expect("round-trip decode of a valid encoded frame failed");
        assert!(encoded.is_empty(), "a complete frame must be consumed");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0], msg);
    }
});
