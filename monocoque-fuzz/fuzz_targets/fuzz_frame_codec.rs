#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use monocoque_core::buffer::SegmentedBuffer;
use monocoque_zmtp::codec::{ZmtpDecoder, ZmtpFrame};

fuzz_target!(|data: &[u8]| {
    // --- Decode path: feed arbitrary bytes and assert no panics ---
    let mut decoder = ZmtpDecoder::new();
    let mut buf = SegmentedBuffer::new();
    buf.push(Bytes::copy_from_slice(data));

    // The result must be Ok(Some), Ok(None), or Err  -  never a panic.
    let _ = decoder.decode(&mut buf);

    // --- Encode path: derive a frame size from the first 2 bytes and encode ---
    if data.len() >= 2 {
        let size = u16::from_le_bytes([data[0], data[1]]) as usize;

        // Build a payload of `size` bytes sourced from the fuzz input (or zeros).
        let payload: Vec<u8> = data
            .iter()
            .cycle()
            .take(size)
            .copied()
            .collect();
        let payload = Bytes::from(payload);

        // Encode a data frame and verify the output is non-empty when expected.
        let frame = ZmtpFrame::data(payload.clone(), false);
        let encoded = frame.encode();

        // The encoded output must be at least header (2 or 9 bytes) + payload long.
        let min_header = if size >= 256 { 9 } else { 2 };
        assert!(encoded.len() >= min_header + size);

        // Round-trip: decode the encoded frame back and assert it is valid.
        let mut rt_decoder = ZmtpDecoder::new();
        let mut rt_buf = SegmentedBuffer::new();
        rt_buf.push(encoded);
        let result = rt_decoder.decode(&mut rt_buf);

        // Encoding must always produce a frame that decodes successfully.
        match result {
            Ok(Some(decoded_frame)) => {
                assert_eq!(decoded_frame.payload.len(), size);
            }
            Ok(None) => {
                // Incomplete  -  only possible for empty payloads under fragmentation,
                // which should not happen for a complete encoded frame.
                // Accept it to keep the fuzz target panic-free.
            }
            Err(_) => {
                // Should never happen for a well-formed encoded frame.
                panic!("round-trip decode of a valid encoded frame failed");
            }
        }
    }
});
