//! Latency benchmarks: round-trip time in microseconds
//!
//! Compares monocoque vs rust-zmq (zmq crate, FFI bindings to libzmq) for latency.
//! Measures: How fast is a single message round-trip?
//!
//! ## Methodology
//!
//! - Server runs on a separate OS thread with its own compio runtime.
//! - The bench thread owns a persistent compio runtime (created once, outside `iter_batched`).
//! - Warmup rounds happen inside the setup closure, so they are not measured.
//! - Each measured iteration: single REQ send + REP echo + REQ recv.

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024];
const WARMUP_ROUNDS: usize = 1_000;

/// Benchmark monocoque REQ/REP latency (single round-trip).
///
/// Server binds on its own OS thread. The bench thread connects, does warmup,
/// then each `iter_batched` iteration measures one round-trip.
fn monocoque_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/monocoque/req_rep");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    // Single runtime reused across all iterations to avoid io_uring resource exhaustion.
    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{size}B")),
            &size,
            |b, _| {
                b.iter_batched(
                    || {
                        // SETUP (not measured): spawn server thread, connect, warmup.
                        let (port_tx, port_rx) = mpsc::channel::<u16>();

                        let server_thread = thread::spawn(move || {
                            let rt = compio::runtime::Runtime::new().unwrap();
                            rt.block_on(async move {
                                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                                let port = listener.local_addr().unwrap().port();
                                port_tx.send(port).unwrap();

                                let (stream, _) = listener.accept().await.unwrap();
                                let mut rep = RepSocket::from_tcp_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(4096, 4096),
                                )
                                .await
                                .unwrap();

                                // Echo WARMUP_ROUNDS + 1 messages, then exit cleanly.
                                for _ in 0..=WARMUP_ROUNDS {
                                    if let Ok(Some(msg)) = rep.recv().await {
                                        if rep.send(msg).await.is_err() {
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            });
                        });

                        let port = port_rx.recv().unwrap();

                        let req = rt.block_on(async {
                            let stream = compio::net::TcpStream::connect(("127.0.0.1", port))
                                .await
                                .unwrap();
                            let mut req = ReqSocket::from_tcp_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(4096, 4096),
                            )
                            .await
                            .unwrap();

                            // Warmup: not measured.
                            for _ in 0..WARMUP_ROUNDS {
                                req.send(vec![payload.clone()]).await.unwrap();
                                req.recv().await.unwrap();
                            }

                            req
                        });

                        (req, server_thread)
                    },
                    |(mut req, server_thread)| {
                        // MEASURED: single round-trip.
                        let payload = payload.clone();
                        rt.block_on(async move {
                            req.send(vec![black_box(payload)]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                            drop(req);
                        });
                        server_thread.join().unwrap();
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark rust-zmq (zmq crate, FFI to libzmq) REQ/REP latency.
///
/// Server binds on its own OS thread. Client connects, does warmup, then each
/// `iter_batched` iteration measures one round-trip.
fn zmq_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/rust_zmq/req_rep");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    for &size in MESSAGE_SIZES {
        let payload = vec![0u8; size];

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{size}B")),
            &size,
            |b, _| {
                b.iter_batched(
                    || {
                        // SETUP (not measured): spawn server thread, connect, warmup.
                        let (endpoint_tx, endpoint_rx) = mpsc::channel::<String>();

                        let server_thread = thread::spawn(move || {
                            let ctx = zmq::Context::new();
                            let rep = ctx.socket(zmq::REP).unwrap();
                            rep.bind("tcp://127.0.0.1:*").unwrap();
                            let endpoint = rep.get_last_endpoint().unwrap().unwrap();
                            endpoint_tx.send(endpoint).unwrap();

                            // Echo WARMUP_ROUNDS + 1 messages, then exit.
                            for _ in 0..=WARMUP_ROUNDS {
                                match rep.recv_bytes(0) {
                                    Ok(msg) => {
                                        if rep.send(&msg, 0).is_err() {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                        });

                        let endpoint = endpoint_rx.recv().unwrap();

                        let ctx = zmq::Context::new();
                        let req = ctx.socket(zmq::REQ).unwrap();
                        req.connect(&endpoint).unwrap();

                        // Warmup rounds establish the connection; not measured.
                        for _ in 0..WARMUP_ROUNDS {
                            req.send(&payload, 0).unwrap();
                            let _ = req.recv_bytes(0).unwrap();
                        }

                        (req, server_thread)
                    },
                    |(req, server_thread)| {
                        // MEASURED: single round-trip.
                        req.send(black_box(&payload), 0).unwrap();
                        let _ = req.recv_bytes(0).unwrap();
                        drop(req);
                        server_thread.join().unwrap();
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, monocoque_req_rep_latency, zmq_req_rep_latency);
criterion_main!(benches);
