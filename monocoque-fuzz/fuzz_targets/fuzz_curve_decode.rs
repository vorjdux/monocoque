#![no_main]

//! Fuzzes CURVE MESSAGE decryption in the production codec.
//!
//! `fuzz_decoder` and `fuzz_frame_codec` build `ZmtpCodec` without a cipher, so
//! the encrypted path (MESSAGE parsing, nonce checks, box opening, the inner
//! flags byte, commands carried inside the box) is fuzzed only here. The codec
//! is the server side of a connection whose keys are already agreed
//! (`CurveMessageCipher::bench_pair`).
//!
//! The first input byte picks the mode:
//! - even: the rest is raw wire bytes fed to the encrypted decoder in chunks.
//!   Almost everything fails authentication, which must be an error, never a
//!   panic.
//! - odd: the rest is a payload the client encrypts; the frame is then
//!   corrupted at one input-chosen byte (or left intact) and decoded. An
//!   intact frame must decode to the same payload; a corrupted one must fail.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use monocoque_core::msg::Msg;
use monocoque_core::mux::out::WriteJob;
use monocoque_core::mux::{Codec, OutQueue};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::security::curve::CurveMessageCipher;
use monocoque_zmtp::wire::ZmtpCodec;

fn codecs() -> (ZmtpCodec, ZmtpCodec) {
    let (client, server) = CurveMessageCipher::bench_pair();
    let opts = SocketOptions::default();
    (
        ZmtpCodec::new(Some(client), &opts),
        ZmtpCodec::new(Some(server), &opts),
    )
}

/// The bytes an `OutQueue` would put on the wire.
fn wire(mut out: OutQueue) -> BytesMut {
    let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
    let mut bytes = BytesMut::new();
    while let Some(job) = out.take_job(&mut spare, &mut iov) {
        match job {
            WriteJob::Contiguous(b) => bytes.extend_from_slice(&b),
            WriteJob::Vectored(segs) => segs.iter().for_each(|s| bytes.extend_from_slice(s)),
        }
        out.complete_job();
    }
    bytes
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let (mut client, mut server) = codecs();
    let mut replies = OutQueue::default();

    if mode & 1 == 0 {
        let chunk = usize::from(mode >> 1) % 64 + 1;
        let mut buf = BytesMut::new();
        for piece in rest.chunks(chunk) {
            buf.extend_from_slice(piece);
            if server.decode(&mut buf, &mut |_| {}, &mut replies).is_err() {
                return;
            }
        }
        return;
    }

    // Odd mode: the last two bytes choose the corruption, the rest is the
    // payload.
    if rest.len() < 2 {
        return;
    }
    let (payload, pick) = rest.split_at(rest.len() - 2);
    let mut out = OutQueue::default();
    if client.encode(&Msg::from(payload.to_vec()), &mut out).is_err() {
        return;
    }
    let mut frame = wire(out);
    let corrupt = pick[0] & 1 == 1;
    if corrupt {
        let i = usize::from(pick[1]) % frame.len();
        frame[i] ^= 0x01 | (pick[0] & 0xfe);
    }
    let mut got = Vec::new();
    let result = server.decode(&mut frame, &mut |m| got.push(m), &mut replies);
    if !corrupt {
        result.expect("an intact CURVE frame must decode");
        assert_eq!(got.len(), 1, "one frame in, one message out");
        assert_eq!(got[0].first().unwrap_or_default(), payload);
    } else if result.is_ok() {
        // A flipped bit may land in a length field and leave the frame
        // incomplete (nothing decoded yet), but it must never yield a message
        // that differs from what was sent.
        assert!(got.iter().all(|m| m.first().unwrap_or_default() == payload));
    }
});
