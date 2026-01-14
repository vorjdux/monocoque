//! High-throughput benchmarks using explicit batching API
//!
//! This benchmark demonstrates optimal throughput using the explicit
//! `send_buffered()` + `flush()` API for power users who need maximum
//! performance. Regular `send()` is simpler but does one I/O per message.
//!
//! ## Pattern
//!
//! Process messages in batches to avoid TCP backpressure:
//! 1. DEALER: Send batch → flush
//! 2. ROUTER: Receive batch → send batch → flush  
//! 3. DEALER: Receive batch
//! 4. Repeat
//!
//! This streaming pattern avoids deadlock while maximizing throughput.

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use monocoque::zmq::{BufferConfig, DealerSocket, RouterSocket};
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024, 4096, 16384];
const TOTAL_MESSAGES: usize = 10_000;
const BATCH_SIZE: usize = 100; // Process in batches to avoid deadlock

/// Benchmark monocoque DEALER/ROUTER pipelined throughput
///
/// Sends all messages first, then receives all replies.
/// This measures maximum throughput without round-trip latency.
fn monocoque_dealer_router_pipelined(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("pipelined/monocoque/dealer_router");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    // Reuse a single runtime for all iterations
    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.throughput(Throughput::Bytes((size * TOTAL_MESSAGES) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let payload = payload.clone(); // Clone for each iteration
                rt.block_on(async move {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let server_addr = listener.local_addr().unwrap();

                    // Router task: process in batches
                    let router_task = compio::runtime::spawn(async move {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut router =
                            RouterSocket::from_tcp_with_config(stream, BufferConfig::large())
                                .await
                                .unwrap();

                        // Process in batches to avoid deadlock
                        const BATCH_SIZE: usize = 100;
                        for _ in 0..(TOTAL_MESSAGES / BATCH_SIZE) {
                            // Receive batch
                            let mut batch = Vec::with_capacity(BATCH_SIZE);
                            for _ in 0..BATCH_SIZE {
                                if let Some(msg) = router.recv().await {
                                    batch.push(msg);
                                }
                            }
                            // Send batch using batching API
                            for msg in batch {
                                router.send_buffered(msg).unwrap();
                            }
                            router.flush().await.unwrap();
                        }
                    });

                    // Dealer task: process in batches
                    let dealer_task = compio::runtime::spawn(async move {
                        let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                        let mut dealer =
                            DealerSocket::from_tcp_with_config(stream, BufferConfig::large())
                                .await
                                .unwrap();

                        const BATCH_SIZE: usize = 100;
                        for _ in 0..(TOTAL_MESSAGES / BATCH_SIZE) {
                            // Send batch using batching API
                            for _ in 0..BATCH_SIZE {
                                dealer
                                    .send_buffered(vec![black_box(payload.clone())])
                                    .unwrap();
                            }
                            dealer.flush().await.unwrap();

                            // Receive batch
                            for _ in 0..BATCH_SIZE {
                                if dealer.recv().await.is_none() {
                                    break;
                                }
                            }
                        }
                    });

                    // Wait for both tasks to complete
                    dealer_task.await;
                    router_task.await;
                });
            });
        });
    }
    group.finish();
}

/// Benchmark zmq.rs (libzmq) pipelined throughput for comparison
///
/// NOTE: Uses fewer messages (1000) to avoid deadlock with blocking I/O
fn zmq_dealer_router_pipelined(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipelined/zmq_rs/dealer_router");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    const ZMQ_MESSAGES: usize = 1000; // Reduced to avoid blocking I/O deadlock

    for &size in MESSAGE_SIZES {
        let payload = vec![0u8; size];

        group.throughput(Throughput::Bytes((size * ZMQ_MESSAGES) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let ctx = zmq::Context::new();

                let router = ctx.socket(zmq::ROUTER).unwrap();
                router.bind("tcp://127.0.0.1:*").unwrap();
                let endpoint = router.get_last_endpoint().unwrap().unwrap();

                std::thread::sleep(Duration::from_millis(10));
                let dealer = ctx.socket(zmq::DEALER).unwrap();
                dealer.connect(&endpoint).unwrap();
                std::thread::sleep(Duration::from_millis(10));

                let router_handle = std::thread::spawn(move || {
                    // Process in batches to avoid TCP buffer deadlock
                    const BATCH_SIZE: usize = 100;
                    for _ in 0..(ZMQ_MESSAGES / BATCH_SIZE) {
                        // Receive batch
                        let mut batch = Vec::with_capacity(BATCH_SIZE);
                        for _ in 0..BATCH_SIZE {
                            if let Ok(msg) = router.recv_bytes(0) {
                                batch.push(msg);
                            }
                        }
                        // Send batch
                        for msg in batch {
                            router.send(&msg, 0).ok();
                        }
                    }
                });

                // Process in batches to avoid TCP buffer deadlock
                const BATCH_SIZE: usize = 100;
                for _ in 0..(ZMQ_MESSAGES / BATCH_SIZE) {
                    // Send batch
                    for _ in 0..BATCH_SIZE {
                        dealer.send(black_box(&payload), 0).unwrap();
                    }
                    // Receive batch
                    for _ in 0..BATCH_SIZE {
                        if dealer.recv_bytes(0).is_err() {
                            break;
                        }
                    }
                }

                router_handle.join().unwrap();
            });
        });
    }
    group.finish();
}

/// Benchmark asymmetric pipelined throughput (large batches)
///
/// Tests extremely large pipeline depths to see maximum capacity.
fn monocoque_extreme_pipeline(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("pipelined/monocoque/extreme");
    group.measurement_time(Duration::from_secs(30));
    group.sample_size(10); // Minimum required by criterion

    let rt = compio::runtime::Runtime::new().unwrap();

    // Test with 100k messages in pipeline
    let extreme_depth = 100_000;
    let size = 64;
    let payload = Bytes::from(vec![0u8; size]);

    group.throughput(Throughput::Bytes((size * extreme_depth) as u64));
    group.bench_function("100k_messages_64B", |b| {
        b.iter(|| {
            rt.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let server_addr = listener.local_addr().unwrap();

                let router_task = compio::runtime::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut router =
                        RouterSocket::from_tcp_with_config(stream, BufferConfig::large())
                            .await
                            .unwrap();

                    // Echo loop: recv + send immediately
                    for _ in 0..extreme_depth {
                        if let Some(msg) = router.recv().await {
                            router.send(msg).await.ok();
                        }
                    }
                });

                let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                let mut dealer = DealerSocket::from_tcp_with_config(stream, BufferConfig::large())
                    .await
                    .unwrap();

                // Send all messages
                for _ in 0..extreme_depth {
                    dealer.send(vec![black_box(payload.clone())]).await.unwrap();
                }

                // Receive all replies
                for _ in 0..extreme_depth {
                    if dealer.recv().await.is_none() {
                        break;
                    }
                }

                router_task.await;
            });
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    monocoque_dealer_router_pipelined,
    // zmq_dealer_router_pipelined,  // Disabled: blocking I/O causes deadlock
    monocoque_extreme_pipeline,
);
criterion_main!(benches);
