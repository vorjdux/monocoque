//! IPC vs TCP comparison benchmarks
//!
//! Compares Unix domain sockets (IPC) vs TCP loopback for latency and throughput.
//!
//! ## Methodology
//!
//! - Server always runs on a separate OS thread with its own compio runtime.
//! - Latency: `iter_batched`, server exits after warmup+1, one round-trip measured.
//! - Throughput: `iter_custom`, PULL side owns the timer (PUSH/PULL one-way).
//! - IPC socket paths include pid + atomic counter to avoid conflicts across iterations.

#[cfg(unix)]
use bytes::Bytes;
#[cfg(unix)]
use compio::net::{TcpListener, UnixListener};
#[cfg(unix)]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
#[cfg(unix)]
use monocoque::zmq::{PullSocket, PushSocket, RepSocket, ReqSocket, SocketOptions};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
const MESSAGE_SIZES: &[usize] = &[64, 256, 1024];
#[cfg(unix)]
const MESSAGE_COUNT: usize = 10_000;
#[cfg(unix)]
const WARMUP_ROUNDS: usize = 1_000;

#[cfg(unix)]
static IPC_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique IPC socket path for each call.
#[cfg(unix)]
fn ipc_path(label: &str) -> String {
    let id = IPC_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "/tmp/monocoque_bench_{}_{}_{}.sock",
        std::process::id(),
        label,
        id
    )
}

#[cfg(unix)]
/// Benchmark monocoque REQ/REP latency over TCP.
///
/// Server on its own thread; client connects, warms up, measures one round-trip.
fn monocoque_tcp_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/tcp_latency");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    // Single runtime reused across iterations.
    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{size}B")),
            &size,
            |b, _| {
                b.iter_batched(
                    || {
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

                            for _ in 0..WARMUP_ROUNDS {
                                req.send(vec![payload.clone()]).await.unwrap();
                                req.recv().await.unwrap();
                            }

                            req
                        });

                        (req, server_thread)
                    },
                    |(mut req, server_thread)| {
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

#[cfg(unix)]
/// Benchmark monocoque REQ/REP latency over IPC (Unix domain sockets).
///
/// Server on its own thread; client connects via a unique socket path, warms up,
/// measures one round-trip.
fn monocoque_ipc_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/ipc_latency");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{size}B")),
            &size,
            |b, _| {
                b.iter_batched(
                    || {
                        let (path_tx, path_rx) = mpsc::channel::<String>();

                        let server_thread = thread::spawn(move || {
                            let rt = compio::runtime::Runtime::new().unwrap();
                            rt.block_on(async move {
                                let path = ipc_path("lat");
                                let _ = std::fs::remove_file(&path);
                                let listener = UnixListener::bind(&path).await.unwrap();
                                path_tx.send(path.clone()).unwrap();

                                let (stream, _) = listener.accept().await.unwrap();
                                let mut rep =
                                    RepSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                        stream,
                                        SocketOptions::default().with_buffer_sizes(4096, 4096),
                                    )
                                    .await
                                    .unwrap();

                                for _ in 0..=WARMUP_ROUNDS {
                                    if let Ok(Some(msg)) = rep.recv().await {
                                        if rep.send(msg).await.is_err() {
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                                let _ = std::fs::remove_file(&path);
                            });
                        });

                        let path = path_rx.recv().unwrap();

                        let req = rt.block_on(async {
                            let stream =
                                compio::net::UnixStream::connect(&path).await.unwrap();
                            let mut req =
                                ReqSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(4096, 4096),
                                )
                                .await
                                .unwrap();

                            for _ in 0..WARMUP_ROUNDS {
                                req.send(vec![payload.clone()]).await.unwrap();
                                req.recv().await.unwrap();
                            }

                            req
                        });

                        (req, server_thread)
                    },
                    |(mut req, server_thread)| {
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

#[cfg(unix)]
/// Benchmark monocoque PUSH/PULL throughput over TCP.
///
/// PULL binds in a separate thread. PUSH connects in the bench thread.
/// Timer on PULL side.
fn monocoque_tcp_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/tcp_throughput");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (port_tx, port_rx) = mpsc::channel::<u16>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();
                    let payload_clone = payload.clone();

                    let pull_thread = thread::spawn(move || {
                        let rt = compio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let port = listener.local_addr().unwrap().port();
                            port_tx.send(port).unwrap();

                            let (stream, _) = listener.accept().await.unwrap();
                            let mut pull = PullSocket::from_tcp_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(16384, 16384),
                            )
                            .await
                            .unwrap();

                            let t0 = std::time::Instant::now();
                            for _ in 0..MESSAGE_COUNT {
                                pull.recv().await.unwrap();
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                        });
                    });

                    let port = port_rx.recv().unwrap();

                    let push_rt = compio::runtime::Runtime::new().unwrap();
                    push_rt.block_on(async move {
                        let mut push = PushSocket::connect(("127.0.0.1", port)).await.unwrap();
                        for _ in 0..MESSAGE_COUNT {
                            push.send(vec![black_box(payload_clone.clone())])
                                .await
                                .unwrap();
                        }
                    });

                    pull_thread.join().unwrap();
                    total += elapsed_rx.recv().unwrap();
                }

                total
            });
        });
    }
    group.finish();
}

#[cfg(unix)]
/// Benchmark monocoque PUSH/PULL throughput over IPC (Unix domain sockets).
///
/// PULL binds in a separate thread. PUSH connects in the bench thread.
/// Timer on PULL side.
fn monocoque_ipc_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/ipc_throughput");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (path_tx, path_rx) = mpsc::channel::<String>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();
                    let payload_clone = payload.clone();

                    let pull_thread = thread::spawn(move || {
                        let rt = compio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let path = ipc_path("tp");
                            let _ = std::fs::remove_file(&path);
                            let listener = UnixListener::bind(&path).await.unwrap();
                            path_tx.send(path.clone()).unwrap();

                            let (stream, _) = listener.accept().await.unwrap();
                            let mut pull =
                                PullSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(16384, 16384),
                                )
                                .await
                                .unwrap();

                            let t0 = std::time::Instant::now();
                            for _ in 0..MESSAGE_COUNT {
                                pull.recv().await.unwrap();
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                            let _ = std::fs::remove_file(&path);
                        });
                    });

                    let path = path_rx.recv().unwrap();

                    let push_rt = compio::runtime::Runtime::new().unwrap();
                    push_rt.block_on(async move {
                        let stream =
                            compio::net::UnixStream::connect(&path).await.unwrap();
                        let mut push =
                            PushSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(16384, 16384),
                            )
                            .await
                            .unwrap();

                        for _ in 0..MESSAGE_COUNT {
                            push.send(vec![black_box(payload_clone.clone())])
                                .await
                                .unwrap();
                        }
                    });

                    pull_thread.join().unwrap();
                    total += elapsed_rx.recv().unwrap();
                }

                total
            });
        });
    }
    group.finish();
}

#[cfg(unix)]
criterion_group!(
    benches,
    monocoque_tcp_latency,
    monocoque_ipc_latency,
    monocoque_tcp_throughput,
    monocoque_ipc_throughput,
);

#[cfg(unix)]
criterion_main!(benches);

#[cfg(not(unix))]
fn main() {
    println!("IPC benchmarks require Unix platform");
}
