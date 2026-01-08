//! Latency benchmarks: round-trip time in microseconds
//!
//! Compares monocoque vs zmq.rs (libzmq bindings) for latency.
//! Measures: How fast is a single message round-trip?
//!
//! Tests the PUBLIC API from `monocoque::zmq` (user-facing ergonomics)

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use monocoque::zmq::{RepSocket, ReqSocket};
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256]; // Reduced for quick testing
const ROUND_TRIPS: usize = 1000; // Multiple round-trips per iteration to amortize runtime creation
const CONNECTIONS: usize = 100; // Multiple connections per iteration for connection benchmark

/// Benchmark monocoque REQ/REP latency (single round-trip)
fn monocoque_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/monocoque/req_rep");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10); // Low sample count like throughput benchmark
    group.warm_up_time(Duration::from_millis(100)); // Minimal warmup - only ~1-2 iterations

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        // Creating/dropping many io_uring runtimes can exhaust kernel resources.
        // Reuse a single runtime for all iterations of this benchmark.
        let rt = compio::runtime::Runtime::new().unwrap();

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{}B", size)),
            &size,
            |b, _| {
                b.iter(|| {
                    // Measured code: full round-trip including runtime
                    // Do multiple round-trips to amortize runtime creation overhead
                    rt.block_on(async {
                        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                        let server_addr = listener.local_addr().unwrap();

                        let server_task = compio::runtime::spawn(async move {
                            let (stream, _) = listener.accept().await.unwrap();
                            let mut rep = RepSocket::from_stream(stream).await.unwrap();
                            for _ in 0..ROUND_TRIPS {
                                if let Some(msg) = rep.recv().await {
                                    rep.send(msg).await.ok();
                                } else {
                                    break;
                                }
                            }
                        });

                        let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                        let mut req = ReqSocket::from_stream(stream).await.unwrap();

                        for _ in 0..ROUND_TRIPS {
                            req.send(vec![black_box(payload.clone())]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                        }

                        server_task.await;
                    })
                });
            },
        );
    }
    group.finish();
}

/// Benchmark zmq.rs (libzmq) REQ/REP latency
fn zmq_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/zmq_rs/req_rep");
    group.measurement_time(Duration::from_secs(15));

    for &size in MESSAGE_SIZES {
        let payload = vec![0u8; size];

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{}B", size)),
            &size,
            |b, _| {
                let ctx = zmq::Context::new();
                let rep = ctx.socket(zmq::REP).unwrap();
                rep.bind("tcp://127.0.0.1:*").unwrap();
                let endpoint = rep.get_last_endpoint().unwrap().unwrap();

                std::thread::spawn(move || loop {
                    match rep.recv_bytes(0) {
                        Ok(msg) => {
                            if rep.send(&msg, 0).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                });

                std::thread::sleep(Duration::from_millis(10));
                let req = ctx.socket(zmq::REQ).unwrap();
                req.connect(&endpoint).unwrap();

                // Warmup
                for _ in 0..100 {
                    req.send(&payload, 0).unwrap();
                    let _ = req.recv_bytes(0).unwrap();
                }

                // Measure single round-trip
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
                        let _ = RepSocket::from_stream(stream).await.unwrap();
                    }
                });

                for _ in 0..CONNECTIONS {
                    let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                    let req = ReqSocket::from_stream(stream).await.unwrap();
                    black_box(req);
                }

                accept_task.await;
            })
        });
    });

    group.finish();
}

/// Benchmark zmq.rs connection establishment latency
fn zmq_connection_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/zmq_rs/connection");
    group.sample_size(10); // Low sample count to avoid "too many open files"
    group.warm_up_time(Duration::from_millis(50)); // Very minimal warmup

    group.bench_function("req_connect", |b| {
        let ctx = zmq::Context::new();
        let rep = ctx.socket(zmq::REP).unwrap();
        rep.bind("tcp://127.0.0.1:*").unwrap();
        let endpoint = rep.get_last_endpoint().unwrap().unwrap();

        b.iter(|| {
            // Create fewer connections per iteration for zmq.rs (50 instead of 100)
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
    zmq_req_rep_latency,
    monocoque_connection_latency
);
// zmq_connection_latency disabled - exhausts file descriptors
criterion_main!(benches);
