#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use monocoque_zmtp::security::PlainCredentials;
use monocoque_zmtp::security::zap::{ZapMechanism, ZapRequest, ZapResponse};
use monocoque_zmtp::security::plain::create_plain_zap_request;

fuzz_target!(|data: &[u8]| {
    // --- Fuzz ZapRequest::decode with arbitrary frame data ---
    // Split the fuzz input into up to 8 frames using the first byte as a
    // frame-count hint, then attempt to decode as a ZAP request.
    if data.len() >= 2 {
        let num_frames = (data[0] as usize % 9) + 1; // 1..=9 frames
        let payload = &data[1..];

        // Distribute remaining bytes roughly evenly across frames.
        let frames: Vec<Bytes> = if num_frames == 0 || payload.is_empty() {
            vec![]
        } else {
            let chunk_size = (payload.len() / num_frames).max(1);
            payload
                .chunks(chunk_size)
                .take(num_frames)
                .map(|c| Bytes::copy_from_slice(c))
                .collect()
        };

        // Must not panic — only Ok or Err.
        let _ = ZapRequest::decode(&frames);
    }

    // --- Fuzz ZapResponse::decode with arbitrary frame data ---
    // A valid ZapResponse requires exactly 6 frames; exercise with arbitrary counts.
    if data.len() >= 7 {
        let frames: Vec<Bytes> = data[1..]
            .chunks(((data.len() - 1) / 6).max(1))
            .take(6)
            .map(|c| Bytes::copy_from_slice(c))
            .collect();

        let _ = ZapResponse::decode(&frames);
    }

    // --- Fuzz ZapMechanism::from_str with arbitrary UTF-8 input ---
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = ZapMechanism::from_str(s);
    }

    // --- Fuzz create_plain_zap_request with arbitrary credentials ---
    // Use the first byte to split the fuzz input into username / password halves.
    if data.len() >= 3 {
        let split = (data[0] as usize % (data.len() - 1)) + 1;
        let username_bytes = &data[1..split.min(data.len())];
        let password_bytes = &data[split.min(data.len())..];

        // from_utf8 lossy — keep going even if not valid UTF-8.
        let username = String::from_utf8_lossy(username_bytes).into_owned();
        let password = String::from_utf8_lossy(password_bytes).into_owned();

        let request = create_plain_zap_request(
            "req-fuzz",
            "domain",
            "127.0.0.1:5555",
            Bytes::from_static(b"identity"),
            username,
            password,
        );

        // Encoding must not panic.
        let encoded = request.encode();
        assert!(!encoded.is_empty());
    }

    // --- Fuzz PlainCredentials construction with arbitrary strings ---
    if data.len() >= 2 {
        let mid = data.len() / 2;
        let username = String::from_utf8_lossy(&data[..mid]).into_owned();
        let password = String::from_utf8_lossy(&data[mid..]).into_owned();

        let _creds = PlainCredentials::new(username.clone(), password.clone());

        // StaticPlainHandler authentication path (synchronous path only,
        // async path requires a runtime which fuzz targets do not use).
        let _ = username.len();
        let _ = password.len();
    }

    // --- Fuzz ZapRequest encode → decode round-trip ---
    if data.len() >= 4 {
        let mid = data.len() / 2;
        let username = String::from_utf8_lossy(&data[..mid]).into_owned();
        let password = String::from_utf8_lossy(&data[mid..]).into_owned();

        let request = create_plain_zap_request(
            "rt",
            "d",
            "0.0.0.0:0",
            Bytes::new(),
            username,
            password,
        );

        let frames = request.encode();
        // Decode should succeed for a properly encoded request.
        match ZapRequest::decode(&frames) {
            Ok(_) => {}
            Err(_) => {
                // Should never fail for a round-tripped request.
                panic!("ZapRequest round-trip decode failed");
            }
        }
    }
});
