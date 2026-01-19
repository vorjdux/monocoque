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
    monocoque::dev_tracing::init_tracing();
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

                            let server_task = compio::runtime::spawn(async move {
                                let (stream, _) = listener.accept().await.unwrap();
                                let mut rep = RepSocket::from_tcp_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(4096, 4096),
                                )
                                .await
                                .unwrap();

                                loop {
                                    if let Some(msg) = rep.recv().await {
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
                            let mut req =
                                ReqSocket::from_tcp_with_options(stream, SocketOptions::default().with_buffer_sizes(4096, 4096))
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
                        rt.block_on(async {
                            req.send(vec![black_box(payload.clone())]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                        });
                        drop(req);
                        rt.block_on(server_task);
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
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/ipc_latency");
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
                            let socket_path = format!("/tmp/monocoque_bench_{}.sock", std::process::id());
                            
                            // Clean up any existing socket
                            let _ = std::fs::remove_file(&socket_path);
                            
                            let listener = UnixListener::bind(&socket_path).await.unwrap();

                            let server_task = compio::runtime::spawn({
                                let socket_path = socket_path.clone();
                                async move {
                                    let (stream, _) = listener.accept().await.unwrap();
                                    let mut rep = RepSocket::<compio::net::UnixStream>::from_unix_stream_with_options(
                                        stream,
                                        SocketOptions::default().with_buffer_sizes(4096, 4096),
                                    )
                                    .await
                                    .unwrap();

                                    loop {
                                        if let Some(msg) = rep.recv().await {
                                            if rep.send(msg).await.is_err() {
                                                break;
                                            }
                                        } else {
                                            break;
                                        }
                                    }
                                    let _ = std::fs::remove_file(&socket_path);
                                }
                            });

                            // Wait for socket to be ready
                            compio::time::sleep(Duration::from_millis(10)).await;

                            let stream = compio::net::UnixStream::connect(&socket_path).await.unwrap();
                            let mut req =
                                ReqSocket::<compio::net::UnixStream>::from_unix_stream_with_options(stream, SocketOptions::default().with_buffer_sizes(4096, 4096))
                                    .await
                                    .unwrap();

                            // Warmup
                            for _ in 0..WARMUP_ROUNDS {
                                req.send(vec![payload.clone()]).await.unwrap();
                                let _ = req.recv().await.unwrap();
                            }

                            (req, server_task, socket_path)
                        })
                    },
                    |(mut req, server_task, _socket_path)| {
                        rt.block_on(async {
                            req.send(vec![black_box(payload.clone())]).await.unwrap();
                            let _ = req.recv().await.unwrap();
                        });
                        drop(req);
                        rt.block_on(server_task);
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

#[cfg(unix)]
/// Benchmark monocoque DEALER/ROUTER throughput over TCP
fn monocoque_tcp_throughput(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/tcp_throughput");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &size,
            |b, _| {
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
                                if let Some(msg) = router.recv().await {
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
                            if dealer.recv().await.is_none() {
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
/// Benchmark monocoque DEALER/ROUTER throughput over IPC (Unix domain sockets)
fn monocoque_ipc_throughput(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("ipc_vs_tcp/monocoque/ipc_throughput");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &size,
            |b, _| {
                b.iter(|| {
                    rt.block_on(async {
                        let socket_path = format!("/tmp/monocoque_bench_{}.sock", std::process::id());
                        
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
                                    if let Some(msg) = router.recv().await {
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
                            if dealer.recv().await.is_none() {
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
