//! Memory allocation benchmarks.
//!
//! Tracks allocation pressure in the hot path: frame construction, Bytes
//! clone-vs-copy, and arena vs heap allocation patterns.
//!
//! Run with: `cargo bench --package monocoque -F zmq --bench allocation`

use bytes::{Buf, Bytes, BytesMut};
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use monocoque_core::msg::Msg;
use std::hint::black_box;

/// `Bytes::copy_from_slice` vs `Bytes::from` (moves ownership)
fn bench_bytes_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("bytes_construction");

    let small = vec![0u8; 64];
    let medium = vec![0u8; 1024];
    let large = vec![0u8; 65536];

    group.throughput(Throughput::Bytes(64));
    group.bench_function("copy_64b", |b| {
        b.iter(|| {
            let b = Bytes::copy_from_slice(black_box(&small));
            black_box(b);
        });
    });

    group.throughput(Throughput::Bytes(1024));
    group.bench_function("copy_1kb", |b| {
        b.iter(|| {
            let b = Bytes::copy_from_slice(black_box(&medium));
            black_box(b);
        });
    });

    group.throughput(Throughput::Bytes(65536));
    group.bench_function("copy_64kb", |b| {
        b.iter(|| {
            let b = Bytes::copy_from_slice(black_box(&large));
            black_box(b);
        });
    });

    // Cloning Bytes is an Arc reference-count bump  -  O(1), no allocation
    let frozen = Bytes::from(vec![0u8; 1024]);
    group.bench_function("clone_arc_1kb", |b| {
        b.iter(|| {
            let c = frozen.clone();
            black_box(c);
        });
    });

    group.finish();
}

/// `BytesMut` reuse vs fresh allocation
fn bench_bytesmut_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("bytesmut_reuse");

    group.bench_function("fresh_8kb", |b| {
        b.iter(|| {
            let buf = BytesMut::with_capacity(8192);
            black_box(buf);
        });
    });

    group.bench_function("clear_and_reuse_8kb", |b| {
        let mut buf = BytesMut::with_capacity(8192);
        b.iter(|| {
            buf.clear();
            buf.extend_from_slice(&[0u8; 128]);
            black_box(buf.len());
        });
    });

    group.finish();
}

/// Read-buffer drain cycle (mimics the codec read path: skip a frame header,
/// then split the body off without copying it)
fn bench_read_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_buffer");

    group.throughput(Throughput::Bytes(1024));
    group.bench_function("header_then_split_1kb", |b| {
        b.iter_batched(
            || BytesMut::from(&[0u8; 1024][..]),
            |mut buf| {
                buf.advance(2);
                let body = buf.split_to(1022).freeze();
                black_box(body.len());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("small_frame_into_inline_msg", |b| {
        let wire = [0u8; 64];
        b.iter(|| {
            let msg = Msg::copy_from_slice(black_box(&wire));
            black_box(msg);
        });
    });

    group.finish();
}

/// Multipart message Vec allocation patterns
fn bench_multipart_alloc(c: &mut Criterion) {
    let mut group = c.benchmark_group("multipart_alloc");
    let frame = Bytes::from(vec![0u8; 256]);

    group.bench_function("2_frame_vec", |b| {
        b.iter(|| {
            let msg: Vec<Bytes> = vec![frame.clone(), frame.clone()];
            black_box(msg);
        });
    });

    group.bench_function("5_frame_vec", |b| {
        b.iter(|| {
            let msg: Vec<Bytes> = vec![
                frame.clone(),
                frame.clone(),
                frame.clone(),
                frame.clone(),
                frame.clone(),
            ];
            black_box(msg);
        });
    });

    group.bench_function("router_envelope_3_frame", |b| {
        // Simulates a ROUTER envelope: [routing_id, empty, payload]
        let id = Bytes::copy_from_slice(b"peer-identity-01");
        let empty = Bytes::new();
        b.iter(|| {
            let msg: Vec<Bytes> = vec![id.clone(), empty.clone(), frame.clone()];
            black_box(msg);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_bytes_construction,
    bench_bytesmut_reuse,
    bench_read_buffer,
    bench_multipart_alloc,
);
criterion_main!(benches);
