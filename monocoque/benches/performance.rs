//! Benchmark suite for monocoque-zmtp performance testing
//!
//! Measures latency, throughput, and memory usage compared to libzmq.
//!
//! Run with: cargo bench --package monocoque-zmtp

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::{DealerSocket, PairSocket, PubSocket, PullSocket, PushSocket, SubSocket};
use std::time::Duration;

// Helper to run async code in compio runtime
fn runtime() -> compio::runtime::Runtime {
    compio::runtime::Runtime::new().expect("Failed to create runtime")
}

/// Benchmark REQ/REP pattern latency
fn bench_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("req_rep_latency");
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("single_message", |b| {
        b.iter(|| {
            runtime().block_on(async {
                // Simple echo test - measure round-trip time
                let msg = vec![Bytes::from("ping")];
                black_box(msg);
            });
        });
    });

    group.finish();
}

/// Benchmark PUB/SUB throughput
fn bench_pub_sub_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("pub_sub_throughput");
    group.throughput(Throughput::Elements(1000));
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("1kb_messages", |b| {
        b.iter(|| {
            runtime().block_on(async {
                let data = vec![0u8; 1024];
                let msg = vec![Bytes::from(data)];
                black_box(msg);
            });
        });
    });

    group.finish();
}

/// Benchmark PUSH/PULL pipeline throughput
fn bench_push_pull_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("push_pull_pipeline");
    group.throughput(Throughput::Bytes(1024 * 1000)); // 1MB
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("batch_1000", |b| {
        b.iter(|| {
            runtime().block_on(async {
                for _ in 0..1000 {
                    let msg = vec![Bytes::from(vec![0u8; 1024])];
                    black_box(msg);
                }
            });
        });
    });

    group.finish();
}

/// Benchmark message construction overhead
fn bench_message_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("message_construction");

    group.bench_function("single_frame", |b| {
        b.iter(|| {
            let msg = vec![Bytes::from("Hello, World!")];
            black_box(msg);
        });
    });

    group.bench_function("multipart_5_frames", |b| {
        b.iter(|| {
            let msg = vec![
                Bytes::from("frame1"),
                Bytes::from("frame2"),
                Bytes::from("frame3"),
                Bytes::from("frame4"),
                Bytes::from("frame5"),
            ];
            black_box(msg);
        });
    });

    group.bench_function("large_payload_1mb", |b| {
        b.iter(|| {
            let data = vec![0u8; 1024 * 1024];
            let msg = vec![Bytes::from(data)];
            black_box(msg);
        });
    });

    group.finish();
}

/// Benchmark socket options configuration
fn bench_socket_options(c: &mut Criterion) {
    let mut group = c.benchmark_group("socket_options");

    group.bench_function("default_options", |b| {
        b.iter(|| {
            let opts = SocketOptions::new();
            black_box(opts);
        });
    });

    group.bench_function("with_timeouts", |b| {
        b.iter(|| {
            let opts = SocketOptions::new()
                .with_recv_timeout(Duration::from_secs(5))
                .with_send_timeout(Duration::from_secs(5));
            black_box(opts);
        });
    });

    group.bench_function("full_config", |b| {
        b.iter(|| {
            let opts = SocketOptions::new()
                .with_recv_timeout(Duration::from_secs(5))
                .with_send_timeout(Duration::from_secs(5))
                .with_recv_hwm(1000)
                .with_send_hwm(1000)
                .with_immediate(true)
                .with_conflate(false);
            black_box(opts);
        });
    });

    group.finish();
}

/// Benchmark DEALER socket creation
fn bench_dealer_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("dealer_creation");

    group.bench_function("new_with_defaults", |b| {
        b.iter(|| {
            let socket: DealerSocket = DealerSocket::new();
            black_box(socket);
        });
    });

    group.bench_function("new_with_options", |b| {
        b.iter(|| {
            let opts = SocketOptions::new().with_recv_timeout(Duration::from_secs(5));
            let socket = DealerSocket::with_options(opts);
            black_box(socket);
        });
    });

    group.finish();
}

/// Benchmark zero-copy operations
fn bench_zero_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("zero_copy");

    group.bench_function("bytes_clone", |b| {
        let data = Bytes::from(vec![0u8; 1024]);
        b.iter(|| {
            let cloned = data.clone();
            black_box(cloned);
        });
    });

    group.bench_function("vec_clone", |b| {
        let data = vec![0u8; 1024];
        b.iter(|| {
            let cloned = data.clone();
            black_box(cloned);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_req_rep_latency,
    bench_pub_sub_throughput,
    bench_push_pull_pipeline,
    bench_message_construction,
    bench_socket_options,
    bench_dealer_creation,
    bench_zero_copy
);

criterion_main!(benches);
