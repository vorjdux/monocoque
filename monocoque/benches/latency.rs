//! Latency benchmarks: round-trip time in microseconds
//!
//! Compares monocoque vs rust-zmq (zmq crate, FFI bindings to libzmq) for latency.
//! Measures: How fast is a single message round-trip?
//!
//! Tests the PUBLIC API from `monocoque::zmq` (user-facing ergonomics)

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024]; // Various message sizes
const WARMUP_ROUNDS: usize = 100; // Warmup iterations
#[allow(dead_code)]
const CONNECTIONS: usize = 100; // Multiple connections per iteration for connection benchmark

/// Benchmark monocoque REQ/REP latency (single round-trip)
///
/// Setup (OUTSIDE measurement):
/// - Runtime creation
/// - Socket connection + ZMTP handshake
/// - Warmup rounds
///
/// Measured (INSIDE iter):
/// - Single send + recv round-trip only
fn monocoque_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/monocoque/req_rep");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100); // More samples for better statistics

    // SETUP: Create runtime ONCE (outside measurement)
    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{}B", size)),
            &size,
            |b, _| {
                // Set up a single connection per measurement batch and reuse it for
                // all `iters` round-trips, timing only the round-trips. Reconnecting
                // per iteration churned through ephemeral ports (each closed socket
                // lingers in TIME_WAIT), which exhausted them and panicked with
                // AddrNotAvailable. This also matches the rust-zmq side, which keeps
                // one persistent connection and measures pure send/recv.
                b.iter_custom(|iters| {
                    let payload = payload.clone();
                    rt.block_on(async move {
                        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                        let server_addr = listener.local_addr().unwrap();

                        // Server echoes warmup + measured messages, then exits cleanly.
                        let total_msgs = WARMUP_ROUNDS as u64 + iters;
                        let server_task = compio::runtime::spawn(async move {
                            let (stream, _) = listener.accept().await.unwrap();
                            let mut rep = RepSocket::from_tcp_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(4096, 4096),
                            )
                            .await
                            .unwrap();

                            for _ in 0..total_msgs {
                                if let Ok(Some(msg)) = rep.recv().await {
                                    if rep.send(msg).await.is_err() {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                        });

                        let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                        let mut req = ReqSocket::from_tcp_with_options(
                            stream,
                            SocketOptions::default().with_buffer_sizes(4096, 4096),
                        )
                        .await
                        .unwrap();

                        // WARMUP: not timed
                        for _ in 0..WARMUP_ROUNDS {
                            req.send(vec![payload.clone()]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                        }

                        // MEASURE: round-trips over the persistent connection
                        let t0 = std::time::Instant::now();
                        for _ in 0..iters {
                            req.send(vec![black_box(payload.clone())]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                        }
                        let total = t0.elapsed();

                        // Teardown
                        drop(req);
                        server_task.await;
                        total
                    })
                });
            },
        );
    }
    group.finish();
}

/// Benchmark rust-zmq (zmq crate, FFI to libzmq) REQ/REP latency
///
/// Setup (OUTSIDE measurement):
/// - Socket creation + connection
/// - Warmup rounds
///
/// Measured (INSIDE iter):
/// - Single send + recv round-trip only
fn zmq_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/rust_zmq/req_rep");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100); // Match monocoque sample count

    for &size in MESSAGE_SIZES {
        let payload = vec![0u8; size];

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{}B", size)),
            &size,
            |b, _| {
                // SETUP: Create sockets ONCE (outside measurement)
                let ctx = zmq::Context::new();
                let rep = ctx.socket(zmq::REP).unwrap();
                rep.bind("tcp://127.0.0.1:*").unwrap();
                let endpoint = rep.get_last_endpoint().unwrap().unwrap();

                std::thread::spawn(move || {
                    while let Ok(msg) = rep.recv_bytes(0) {
                        if rep.send(&msg, 0).is_err() {
                            break;
                        }
                    }
                });

                std::thread::sleep(Duration::from_millis(10));
                let req = ctx.socket(zmq::REQ).unwrap();
                req.connect(&endpoint).unwrap();

                // WARMUP: Do warmup rounds OUTSIDE measurement (match monocoque)
                for _ in 0..WARMUP_ROUNDS {
                    req.send(&payload, 0).unwrap();
                    let _ = req.recv_bytes(0).unwrap();
                }

                // MEASURED: Only the actual message round-trip
                b.iter(|| {
                    req.send(black_box(&payload), 0).unwrap();
                    let _ = req.recv_bytes(0).unwrap();
                });
            },
        );
    }
    group.finish();
}

/// Benchmark monocoque connection establishment latency
#[allow(dead_code)]
fn monocoque_connection_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/monocoque/connection");
    group.sample_size(10); // Low sample count like throughput benchmark
    group.warm_up_time(Duration::from_millis(100)); // Minimal warmup

    // Reuse a single runtime for all iterations of this benchmark.
    let rt = compio::runtime::Runtime::new().unwrap();

    group.bench_function("req_connect", |b| {
        b.iter(|| {
            rt.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let server_addr = listener.local_addr().unwrap();

                let accept_task = compio::runtime::spawn(async move {
                    for _ in 0..CONNECTIONS {
                        let (stream, _) = listener.accept().await.unwrap();
                        let _ = RepSocket::from_tcp_with_options(
                            stream,
                            SocketOptions::default().with_buffer_sizes(4096, 4096),
                        )
                        .await
                        .unwrap();
                    }
                });

                for _ in 0..CONNECTIONS {
                    let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                    let req = ReqSocket::from_tcp_with_options(
                        stream,
                        SocketOptions::default().with_buffer_sizes(4096, 4096),
                    )
                    .await
                    .unwrap();
                    black_box(req);
                }

                accept_task.await;
            })
        });
    });

    group.finish();
}

/// Benchmark rust-zmq connection establishment latency
#[allow(dead_code)]
fn zmq_connection_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/rust_zmq/connection");
    group.sample_size(10); // Low sample count to avoid "too many open files"
    group.warm_up_time(Duration::from_millis(50)); // Very minimal warmup

    group.bench_function("req_connect", |b| {
        let ctx = zmq::Context::new();
        let rep = ctx.socket(zmq::REP).unwrap();
        rep.bind("tcp://127.0.0.1:*").unwrap();
        let endpoint = rep.get_last_endpoint().unwrap().unwrap();

        b.iter(|| {
            // Create fewer connections per iteration for rust-zmq (50 instead of 100)
            for _ in 0..50 {
                let req = ctx.socket(zmq::REQ).unwrap();
                req.connect(black_box(&endpoint)).unwrap();
                drop(req); // Explicitly drop to close socket
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    monocoque_req_rep_latency,
    zmq_req_rep_latency // monocoque_connection_latency - disabled, needs proper resource cleanup
);
// zmq_connection_latency disabled - exhausts file descriptors
criterion_main!(benches);
