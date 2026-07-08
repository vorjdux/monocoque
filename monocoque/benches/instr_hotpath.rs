//! Instruction-count gate for the IO-free CPU hot path.
//!
//! Wall-clock benches (throughput/latency) are honest but noisy; they cannot
//! catch a small, consistent instruction-count regression in the framing code.
//! This bench pins the CPU hot path under callgrind, which counts instructions
//! deterministically (no timing noise), so a regression shows as a hard number.
//!
//! It covers only the IO-free work: frame encode, frame decode, the segmented
//! read buffer's take/advance, and the vectored-write slice builder. Socket
//! send/recv is deliberately excluded: syscalls under valgrind are slow and
//! their counts are noisy, which is exactly what the wall-clock benches measure
//! instead.
//!
//! Run under valgrind (not part of the default `cargo bench`):
//! `cargo bench --bench instr_hotpath --features zmq`
//! Requires `valgrind` and the `gungraun-runner` binary on PATH; CI installs
//! both. See `.github/workflows/instr-bench.yml`.

use bytes::{Bytes, BytesMut};
use gungraun::{
    Callgrind, EventKind, LibraryBenchmarkConfig, library_benchmark, library_benchmark_group, main,
};
use monocoque_core::buffer::SegmentedBuffer;
use monocoque_core::io::with_vectored_slices;
use monocoque_zmtp::codec::{ZmtpDecoder, encode_multipart, encode_single};
use std::hint::black_box;

// ── setup helpers (run outside the counted region) ──────────────────────────

/// A payload of `len` zero bytes.
fn payload(len: usize) -> Bytes {
    Bytes::from(vec![0u8; len])
}

/// A three-frame multipart message (MORE flags + a long middle frame).
fn multipart_msg() -> Vec<Bytes> {
    vec![
        Bytes::from_static(b"topic"),
        Bytes::from(vec![0xAB; 300]),
        Bytes::from_static(b"tail"),
    ]
}

/// A decoder plus a segmented buffer preloaded with `count` encoded 64-byte
/// frames, ready to be drained.
fn decode_input(count: usize) -> (ZmtpDecoder, SegmentedBuffer) {
    let mut wire = BytesMut::new();
    let part = Bytes::from(vec![0u8; 64]);
    for _ in 0..count {
        encode_single(&part, &mut wire);
    }
    let mut src = SegmentedBuffer::new();
    src.push(wire.freeze());
    (ZmtpDecoder::new(), src)
}

/// A segmented buffer holding `chunks` pushed 1 KiB segments.
fn segmented(chunks: usize) -> SegmentedBuffer {
    let mut src = SegmentedBuffer::new();
    for _ in 0..chunks {
        src.push(Bytes::from(vec![0u8; 1024]));
    }
    src
}

// ── benchmarks (function body is the counted region) ────────────────────────

// Encode one data frame (short = 1-byte header, long = 8-byte length header).
#[library_benchmark]
#[bench::short(args = (64), setup = payload)]
#[bench::long(args = (300), setup = payload)]
fn encode_one(part: Bytes) -> BytesMut {
    let mut buf = BytesMut::with_capacity(part.len() + 9);
    encode_single(black_box(&part), &mut buf);
    black_box(buf)
}

// Encode a three-frame multipart message.
#[library_benchmark]
#[bench::three(setup = multipart_msg)]
fn encode_multi(msg: Vec<Bytes>) -> BytesMut {
    let mut buf = BytesMut::with_capacity(512);
    encode_multipart(black_box(&msg), &mut buf);
    black_box(buf)
}

// Decode every frame out of a preloaded segmented buffer.
#[library_benchmark]
#[bench::frames(args = (64), setup = decode_input)]
fn decode_frames(input: (ZmtpDecoder, SegmentedBuffer)) -> usize {
    let (mut decoder, mut src) = input;
    let mut n = 0usize;
    while let Ok(Some(frame)) = decoder.decode(&mut src) {
        n += black_box(frame.payload.len());
    }
    black_box(n)
}

// Take 64-byte slices off the front until the buffer drains (zero-copy slice
// bookkeeping across segment boundaries).
#[library_benchmark]
#[bench::take(args = (16), setup = segmented)]
fn segbuf_take(mut src: SegmentedBuffer) -> usize {
    let mut taken = 0usize;
    while let Some(bytes) = src.take_bytes(64) {
        taken += black_box(bytes.len());
    }
    black_box(taken)
}

// Advance past the buffer in 64-byte steps (drop bookkeeping without slicing).
#[library_benchmark]
#[bench::advance(args = (16), setup = segmented)]
fn segbuf_advance(mut src: SegmentedBuffer) -> usize {
    let mut steps = 0usize;
    while src.len() >= 64 {
        src.advance(64);
        steps += 1;
    }
    black_box(steps)
}

// Build `IoSlice`s over an owned vectored buffer for one writev.
#[library_benchmark]
#[bench::frames(setup = multipart_msg)]
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
        segbuf_take,
        segbuf_advance,
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
