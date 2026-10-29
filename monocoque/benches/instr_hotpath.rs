//! Instruction-count gate for the IO-free CPU hot path.
//!
//! Wall-clock benches (throughput/latency) are honest but noisy; they cannot
//! catch a small, consistent instruction-count regression in the framing code.
//! This bench pins the CPU hot path under callgrind, which counts instructions
//! deterministically (no timing noise), so a regression shows as a hard number.
//!
//! It covers only the IO-free work: frame encode, frame decode straight out of
//! a read buffer (inline small frames and zero-copy large ones), and the
//! vectored-write slice builder. Socket send/recv is deliberately excluded:
//! syscalls under valgrind are slow and their counts are noisy, which is
//! exactly what the wall-clock benches measure instead.
//!
//! Run under valgrind (not part of the default `cargo bench`):
//! `cargo bench --bench instr_hotpath --features zmq`
//! Requires `valgrind` and the `gungraun-runner` binary on PATH; CI installs
//! both. See `.github/workflows/instr-bench.yml`.

use bytes::{Bytes, BytesMut};
use gungraun::{
    Callgrind, EventKind, LibraryBenchmarkConfig, library_benchmark, library_benchmark_group, main,
};
use monocoque_core::io::with_vectored_slices;
use monocoque_core::msg::Msg;
use monocoque_core::mux::{Codec, OutQueue};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::wire::{ZmtpCodec, encode_plain};
use std::hint::black_box;

// ── setup helpers (run outside the counted region) ──────────────────────────

/// A single-frame message of `len` zero bytes.
fn payload(len: usize) -> Msg {
    Msg::from(Bytes::from(vec![0u8; len]))
}

/// The frames of a three-frame multipart message (MORE flags + a long middle
/// frame).
fn multipart_frames() -> Vec<Bytes> {
    vec![
        Bytes::from_static(b"topic"),
        Bytes::from(vec![0xAB; 300]),
        Bytes::from_static(b"tail"),
    ]
}

/// A three-frame multipart message.
fn multipart_msg() -> Msg {
    multipart_frames().into_iter().collect()
}

/// A codec plus a read buffer preloaded with `count` encoded frames of `len`
/// bytes each, ready to be drained.
fn decode_input(count: usize, len: usize) -> (ZmtpCodec, BytesMut) {
    let mut wire = BytesMut::new();
    let part = payload(len);
    for _ in 0..count {
        encode_plain(&part, &mut wire);
    }
    (ZmtpCodec::new(None, &SocketOptions::default()), wire)
}

// ── benchmarks (function body is the counted region) ────────────────────────

// Encode one data frame (short = 1-byte header, long = 8-byte length header).
#[library_benchmark]
#[bench::short(args = (64), setup = payload)]
#[bench::long(args = (300), setup = payload)]
fn encode_one(msg: Msg) -> BytesMut {
    let mut buf = BytesMut::with_capacity(msg.byte_len() + 9);
    encode_plain(black_box(&msg), &mut buf);
    black_box(buf)
}

// Encode a three-frame multipart message.
#[library_benchmark]
#[bench::three(setup = multipart_msg)]
fn encode_multi(msg: Msg) -> BytesMut {
    let mut buf = BytesMut::with_capacity(512);
    encode_plain(black_box(&msg), &mut buf);
    black_box(buf)
}

// Decode every message out of a preloaded read buffer. 16-byte frames are
// copied inline into the message; 1 KiB frames are split off without a copy.
#[library_benchmark]
#[bench::inline(args = (64, 16), setup = decode_input)]
#[bench::shared(args = (16, 1024), setup = decode_input)]
fn decode_frames(input: (ZmtpCodec, BytesMut)) -> usize {
    let (mut codec, mut src) = input;
    let mut replies = OutQueue::default();
    let mut n = 0usize;
    let _ = codec.decode(
        &mut src,
        &mut |msg| n += black_box(msg.byte_len()),
        &mut replies,
    );
    black_box(n)
}

// Build `IoSlice`s over an owned vectored buffer for one writev.
#[library_benchmark]
#[bench::frames(setup = multipart_frames)]
fn vectored_slices(bufs: Vec<Bytes>) -> usize {
    with_vectored_slices(black_box(&bufs), |slices| {
        black_box(slices.iter().map(|s| s.len()).sum())
    })
}

library_benchmark_group!(
    name = hotpath;
    benchmarks =
        encode_one,
        encode_multi,
        decode_frames,
        vectored_slices,
);

// Fail the run when any benchmark's instruction count (Ir) rises more than 5%
// above the stored baseline: margin enough to absorb toolchain jitter, tight
// enough to catch a real hot-path regression. CI persists the baseline in the
// cached target dir, so a PR is compared against it.
main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().soft_limits([(EventKind::Ir, 5.0)]));
    library_benchmark_groups = hotpath
);
