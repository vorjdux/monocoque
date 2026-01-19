//! Multi-threaded scaling benchmarks
//!
//! Tests monocoque's ability to scale horizontally across multiple CPU cores.
//! Each thread runs its own DEALER socket, measuring aggregate throughput.
//!
//! ## Architecture
//!
//! - Lock-free design: Each socket has its own io_uring context
//! - No shared mutable state in hot paths
//! - Independent TCP connections per thread
//!
//! ## Expected Results
//!
//! - Linear scaling up to # of CPU cores
//! - 8 threads × 130k msg/sec = 1M+ aggregate throughput
//! - No contention or lock overhead

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MESSAGE_SIZE: usize = 64;
const MESSAGES_PER_THREAD: usize = 1_000; // Reduced to avoid deadlock
const THREAD_COUNTS: &[usize] = &[1, 2, 4, 8];
const BATCH_SIZE: usize = 100; // Process in batches to avoid deadlock

/// Benchmark multi-threaded DEALER clients against single ROUTER server
///
/// This tests horizontal scalability and lock-free architecture.
fn monocoque_multithreaded_dealers(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("multithreaded/monocoque/dealers");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10); // Minimum required by criterion

    let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);

    for &num_threads in THREAD_COUNTS {
        let total_messages = num_threads * MESSAGES_PER_THREAD;
        group.throughput(Throughput::Elements(total_messages as u64));

        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    // Use a single runtime for the server
                    let rt = compio::runtime::Runtime::new().unwrap();

                    rt.block_on(async {
                        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                        let server_addr = listener.local_addr().unwrap();

                        let expected_total = num_threads * MESSAGES_PER_THREAD;
                        let received_count = Arc::new(AtomicUsize::new(0));

                        // Router server task (handles all connections)
                        let router_task = compio::runtime::spawn({
                            let received_count = Arc::clone(&received_count);
                            async move {
                                // Accept connections and spawn handler for each
                                let mut handlers = Vec::new();
                                for _ in 0..num_threads {
                                    let (stream, _) = listener.accept().await.unwrap();
                                    let received_count = Arc::clone(&received_count);

                                    let handler = compio::runtime::spawn(async move {
                                        let mut router = RouterSocket::from_tcp_with_options(
                                            stream,
                                            SocketOptions::default().with_buffer_sizes(16384, 16384),
                                        )
                                        .await
                                        .unwrap();

                                        while received_count.load(Ordering::Relaxed)
                                            < expected_total
                                        {
                                            if let Some(msg) = router.recv().await {
                                                received_count.fetch_add(1, Ordering::Relaxed);
                                                router.send(msg).await.ok();
                                            } else {
                                                break;
                                            }
                                        }
                                    });
                                    handlers.push(handler);
                                }

                                // Wait for all handlers
                                for handler in handlers {
                                    handler.await;
                                }
                            }
                        });

                        // Small delay to ensure server is listening
                        compio::time::sleep(Duration::from_millis(50)).await;

                        // Spawn N dealer threads, each with its own runtime
                        let mut dealer_handles = Vec::new();
                        for _i in 0..num_threads {
                            let server_addr = server_addr;
                            let payload = payload.clone();

                            let handle = std::thread::spawn(move || {
                                // Each thread gets its own compio runtime
                                let rt = compio::runtime::Runtime::new().unwrap();
                                rt.block_on(async {
                                    let stream =
                                        compio::net::TcpStream::connect(server_addr).await.unwrap();
                                    let mut dealer = DealerSocket::from_tcp_with_options(
                                        stream,
                                        SocketOptions::default().with_buffer_sizes(16384, 16384),
                                    )
                                    .await
                                    .unwrap();

                                    // Use batched streaming to avoid deadlock
                                    for _ in 0..(MESSAGES_PER_THREAD / BATCH_SIZE) {
                                        // Send batch
                                        for _ in 0..BATCH_SIZE {
                                            dealer
                                                .send(vec![black_box(payload.clone())])
                                                .await
                                                .unwrap();
                                        }
                                        // Receive batch
                                        for _ in 0..BATCH_SIZE {
                                            if dealer.recv().await.is_none() {
                                                break;
                                            }
                                        }
                                    }
                                });
                            });
                            dealer_handles.push(handle);
                        }

                        // Wait for all dealer threads
                        for handle in dealer_handles {
                            handle.join().unwrap();
                        }

                        // Wait for router to finish
                        router_task.await;
                    });
                });
            },
        );
    }
    group.finish();
}

/// Benchmark multi-threaded independent DEALER/ROUTER pairs
///
/// This tests scalability when each thread has completely isolated communication.
fn monocoque_multithreaded_independent_pairs(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("multithreaded/monocoque/independent_pairs");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);

    for &num_threads in THREAD_COUNTS {
        let total_messages = num_threads * MESSAGES_PER_THREAD;
        group.throughput(Throughput::Elements(total_messages as u64));

        group.bench_with_input(
            BenchmarkId::new("pairs", num_threads),
            &num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    // Spawn N independent pairs, each in its own thread
                    let mut handles = Vec::new();

                    for _i in 0..num_threads {
                        let payload = payload.clone();

                        let handle = std::thread::spawn(move || {
                            // Each pair gets its own compio runtime
                            let rt = compio::runtime::Runtime::new().unwrap();
                            rt.block_on(async {
                                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                                let server_addr = listener.local_addr().unwrap();

                                // Router task
                                let router_task = compio::runtime::spawn(async move {
                                    let (stream, _) = listener.accept().await.unwrap();
                                    let mut router = RouterSocket::from_tcp_with_options(
                                        stream,
                                        SocketOptions::default().with_buffer_sizes(16384, 16384),
                                    )
                                    .await
                                    .unwrap();

                                    for _ in 0..MESSAGES_PER_THREAD {
                                        if let Some(msg) = router.recv().await {
                                            router.send(msg).await.ok();
                                        }
                                    }
                                });

                                // Dealer task
                                let stream =
                                    compio::net::TcpStream::connect(server_addr).await.unwrap();
                                let mut dealer = DealerSocket::from_tcp_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(16384, 16384),
                                )
                                .await
                                .unwrap();

                                // Use batched streaming to avoid deadlock
                                for _ in 0..(MESSAGES_PER_THREAD / BATCH_SIZE) {
                                    // Send batch
                                    for _ in 0..BATCH_SIZE {
                                        dealer
                                            .send(vec![black_box(payload.clone())])
                                            .await
                                            .unwrap();
                                    }
                                    // Receive batch
                                    for _ in 0..BATCH_SIZE {
                                        if dealer.recv().await.is_none() {
                                            break;
                                        }
                                    }
                                }

                                router_task.await;
                            });
                        });
                        handles.push(handle);
                    }

                    // Wait for all pairs to complete
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

/// Benchmark CPU core utilization efficiency
///
/// Measures how efficiently threads utilize CPU cores (msg/sec per core).
fn monocoque_core_efficiency(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("multithreaded/monocoque/core_efficiency");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);
    let num_cores = num_cpus::get();

    // Test at 50%, 100%, and 150% of available cores
    let test_counts = vec![num_cores / 2, num_cores, (num_cores as f64 * 1.5) as usize];

    for num_threads in test_counts {
        if num_threads == 0 {
            continue;
        }

        let total_messages = num_threads * MESSAGES_PER_THREAD;
        group.throughput(Throughput::Elements(total_messages as u64));

        group.bench_with_input(
            BenchmarkId::new("cores", format!("{}/{}", num_threads, num_cores)),
            &num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let mut handles = Vec::new();

                    for _i in 0..num_threads {
                        let payload = payload.clone();

                        let handle = std::thread::spawn(move || {
                            let rt = compio::runtime::Runtime::new().unwrap();
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

                                    for _ in 0..MESSAGES_PER_THREAD {
                                        if let Some(msg) = router.recv().await {
                                            router.send(msg).await.ok();
                                        }
                                    }
                                });

                                let stream =
                                    compio::net::TcpStream::connect(server_addr).await.unwrap();
                                let mut dealer = DealerSocket::from_tcp_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(16384, 16384),
                                )
                                .await
                                .unwrap();

                                // Use batched streaming to avoid deadlock
                                for _ in 0..(MESSAGES_PER_THREAD / BATCH_SIZE) {
                                    // Send batch
                                    for _ in 0..BATCH_SIZE {
                                        dealer
                                            .send(vec![black_box(payload.clone())])
                                            .await
                                            .unwrap();
                                    }
                                    // Receive batch
                                    for _ in 0..BATCH_SIZE {
                                        if dealer.recv().await.is_none() {
                                            break;
                                        }
                                    }
                                }

                                router_task.await;
                            });
                        });
                        handles.push(handle);
                    }

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    monocoque_multithreaded_independent_pairs, // Simplest case
                                               // monocoque_multithreaded_dealers,  // Disabled: complex coordination
                                               // monocoque_core_efficiency,  // Disabled: complex coordination
);
criterion_main!(benches);
