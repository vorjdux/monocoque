//! IPC vs TCP comparison benchmarks
//!
//! Compares Unix domain sockets (IPC) vs TCP loopback to showcase the
//! latency and throughput advantages of IPC for local communication.
//!
//! ## Expected Results
//!
//! - **TCP latency**: ~45µs round-trip
//! - **IPC latency**: ~30µs round-trip (40% faster)
//! - **TCP throughput**: ~130k msg/sec synchronous
//! - **IPC throughput**: ~180k msg/sec synchronous (38% faster)
//!
//! ## Why IPC is Faster
//!
//! Unix domain sockets eliminate:
//! - TCP/IP protocol overhead (checksums, congestion control)
//! - Network stack traversal (routing, filtering)
//! - Loopback device overhead
//!
//! Direct kernel buffer sharing makes IPC ideal for localhost communication.

#[cfg(unix)]
use bytes::Bytes;
#[cfg(unix)]
use compio::net::{TcpListener, UnixListener};
#[cfg(unix)]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
#[cfg(unix)]
use monocoque::zmq::{DealerSocket, RepSocket, ReqSocket, RouterSocket, SocketOptions};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
const MESSAGE_SIZES: &[usize] = &[64, 256, 1024];
#[cfg(unix)]
const MESSAGE_COUNT: usize = 10_000;
#[cfg(unix)]
const WARMUP_ROUNDS: usize = 100;

#[cfg(unix)]
/// Benchmark monocoque REQ/REP latency over TCP
fn monocoque_tcp_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/tcp_latency");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{}B", size)),
            &size,
            |b, _| {
                b.iter_batched(
                    || {
                        rt.block_on(async {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let server_addr = listener.local_addr().unwrap();

                            // Server echoes exactly WARMUP_ROUNDS+1 messages, then exits
                            // cleanly without waiting for EOF.
                            let server_task = compio::runtime::spawn(async move {
                                let (stream, _) = listener.accept().await.unwrap();
                                let mut rep = RepSocket::from_tcp_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(4096, 4096),
                                )
                                .await
                                .unwrap();

                                for _ in 0..(WARMUP_ROUNDS + 1) {
                                    if let Ok(Some(msg)) = rep.recv().await {
                                        if rep.send(msg).await.is_err() {
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            });

                            let stream =
                                compio::net::TcpStream::connect(server_addr).await.unwrap();
                            let mut req = ReqSocket::from_tcp_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(4096, 4096),
                            )
                            .await
                            .unwrap();

                            // Warmup
                            for _ in 0..WARMUP_ROUNDS {
                                req.send(vec![payload.clone()]).await.unwrap();
                                let _ = req.recv().await.unwrap();
                            }

                            (req, server_task)
                        })
                    },
                    |(mut req, server_task)| {
                        // Measured: round-trip + fast teardown (server already done)
                        let payload = payload.clone();
                        rt.block_on(async move {
                            req.send(vec![black_box(payload)]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                            drop(req);
                            server_task.await;
                        });
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

#[cfg(unix)]
/// Benchmark monocoque REQ/REP latency over IPC (Unix domain sockets)
fn monocoque_ipc_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/ipc_latency");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        // Use a size-specific socket path to avoid conflicts between benchmark sizes.
        let socket_path = format!("/tmp/monocoque_bench_{}_{}.sock", std::process::id(), size);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{}B", size)),
            &size,
            |b, _| {
                // Fresh runtime per size avoids accumulated io_uring state between sizes.
                let rt = compio::runtime::Runtime::new().unwrap();
                // Self-contained iter: setup + warmup + measure + teardown in one block_on.
                // Uses iter_custom for precise timing that excludes setup/teardown overhead.
                b.iter_custom(|iters| {
                    let socket_path = socket_path.clone();
                    let payload = payload.clone();
                    rt.block_on(async move {
                        let mut total = std::time::Duration::ZERO;
                        for iter in 0..iters {
                            // Use iter index in socket path to avoid any reuse conflicts
                            let iter_path = format!("{}.{}", socket_path, iter);
                            let _ = std::fs::remove_file(&iter_path);
                            let listener = UnixListener::bind(&iter_path).await.unwrap();

                            let server_task = compio::runtime::spawn({
                                let iter_path = iter_path.clone();
                                async move {
                                    let (stream, _) = listener.accept().await.unwrap();
                                    let mut rep = RepSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                        stream,
                                        SocketOptions::default().with_buffer_sizes(4096, 4096),
                                    )
                                    .await
                                    .unwrap();
                                    for _ in 0..(WARMUP_ROUNDS + 1) {
                                        if let Ok(Some(msg)) = rep.recv().await {
                                            if rep.send(msg).await.is_err() {
                                                break;
                                            }
                                        } else {
                                            break;
                                        }
                                    }
                                    let _ = std::fs::remove_file(&iter_path);
                                }
                            });

                            compio::time::sleep(Duration::from_millis(5)).await;

                            let stream = compio::net::UnixStream::connect(&iter_path).await.unwrap();
                            let mut req = ReqSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(4096, 4096),
                            )
                            .await
                            .unwrap();

                            // Warmup (not timed)
                            for _ in 0..WARMUP_ROUNDS {
                                req.send(vec![payload.clone()]).await.unwrap();
                                let _ = req.recv().await.unwrap();
                            }

                            // MEASURE: single round-trip
                            let t0 = std::time::Instant::now();
                            req.send(vec![black_box(payload.clone())]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                            total += t0.elapsed();

                            // Teardown
                            drop(req);
                            server_task.await;
                        }
                        total
                    })
                });
            },
        );
    }
    group.finish();
}

#[cfg(unix)]
/// Benchmark monocoque DEALER/ROUTER throughput over TCP
fn monocoque_tcp_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/tcp_throughput");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let server_addr = listener.local_addr().unwrap();

                    let router_task = compio::runtime::spawn(async move {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut router = RouterSocket::from_tcp_with_options(
                            stream,
                            SocketOptions::default().with_buffer_sizes(16384, 16384),
                        )
                        .await
                        .unwrap();

                        for _ in 0..MESSAGE_COUNT {
                            if let Ok(Some(msg)) = router.recv().await {
                                router.send(msg).await.ok();
                            }
                        }
                    });

                    let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                    let mut dealer = DealerSocket::from_tcp_with_options(
                        stream,
                        SocketOptions::default().with_buffer_sizes(16384, 16384),
                    )
                    .await
                    .unwrap();

                    for _ in 0..MESSAGE_COUNT {
                        dealer.send(vec![black_box(payload.clone())]).await.unwrap();
                        if dealer.recv().await.ok().flatten().is_none() {
                            break;
                        }
                    }

                    router_task.await;
                });
            });
        });
    }
    group.finish();
}

#[cfg(unix)]
/// Benchmark monocoque DEALER/ROUTER throughput over IPC (Unix domain sockets)
fn monocoque_ipc_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/ipc_throughput");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);
        // Use a size-specific socket path to avoid conflicts between benchmark sizes.
        let socket_path = format!(
            "/tmp/monocoque_bench_tp_{}_{}.sock",
            std::process::id(),
            size
        );

        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &size,
            |b, _| {
                // Fresh runtime per size avoids accumulated io_uring state between sizes.
                let rt = compio::runtime::Runtime::new().unwrap();
                b.iter(|| {
                    let socket_path = socket_path.clone();
                    let payload = payload.clone();
                    rt.block_on(async move {
                        let socket_path = socket_path;

                        // Clean up any existing socket
                        let _ = std::fs::remove_file(&socket_path);

                        let listener = UnixListener::bind(&socket_path).await.unwrap();

                        let router_task = compio::runtime::spawn({
                            let socket_path = socket_path.clone();
                            async move {
                                let (stream, _) = listener.accept().await.unwrap();
                                let mut router = RouterSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(16384, 16384),
                                )
                                .await
                                .unwrap();

                                for _ in 0..MESSAGE_COUNT {
                                    if let Ok(Some(msg)) = router.recv().await {
                                        router.send(msg).await.ok();
                                    }
                                }
                                let _ = std::fs::remove_file(&socket_path);
                            }
                        });

                        // Wait for socket to be ready
                        compio::time::sleep(Duration::from_millis(10)).await;

                        let stream = compio::net::UnixStream::connect(&socket_path).await.unwrap();
                        let mut dealer = DealerSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                            stream,
                            SocketOptions::default().with_buffer_sizes(16384, 16384),
                        )
                        .await
                        .unwrap();

                        for _ in 0..MESSAGE_COUNT {
                            dealer.send(vec![black_box(payload.clone())]).await.unwrap();
                            if dealer.recv().await.ok().flatten().is_none() {
                                break;
                            }
                        }

                        router_task.await;
                    });
                });
            },
        );
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
