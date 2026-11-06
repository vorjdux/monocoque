//! Multi-threaded scaling benchmarks
//!
//! Tests monocoque's ability to scale horizontally across multiple CPU cores.
//! Each thread runs its own DEALER socket, measuring aggregate throughput.
//!
//! ## Architecture
//!
//! - Each thread runs its own runtime; sockets on a thread share that
//!   thread's runtime, and each socket is driven by one multiplexer.
//! - No shared mutable state between threads in the hot path.
//! - Independent TCP connections per thread.
//!
//! Only `monocoque_multithreaded_independent_pairs` is registered: N threads,
//! each with its own DEALER/ROUTER pair, so aggregate throughput should scale
//! with cores until the machine runs out of them.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

// Identifies which runtime backend this build benchmarks, so compio, tokio, and smol
// results land under distinct criterion ids instead of overwriting each other.
const BENCH_BACKEND: &str = if cfg!(feature = "runtime-tokio") {
    "tokio"
} else if cfg!(feature = "runtime-smol") {
    "smol"
} else {
    "compio"
};
use monocoque::zmq::{DealerSocket, Msg, RouterSocket, SocketOptions};
use std::hint::black_box;
use std::time::Duration;

const MESSAGE_SIZE: usize = 64;
const MESSAGES_PER_THREAD: usize = 1_000;
const THREAD_COUNTS: &[usize] = &[1, 2, 4, 8];
/// Messages in flight per round: each side sends a batch, then receives the
/// echoes, so neither side's HWM fills while the other waits on it.
const BATCH_SIZE: usize = 100;

/// Coalesced sends are held until `flush`, so each batch goes out in one write.
fn coalescing() -> SocketOptions {
    SocketOptions::default().with_write_coalescing(true)
}

/// Wait until `dealer` has a handshaken peer, so timing starts on a live
/// connection.
async fn wait_connected(dealer: &DealerSocket) {
    while dealer.peer_count() == 0 {
        monocoque::rt::sleep(Duration::from_millis(1)).await;
    }
}

/// Echo `total` messages back to their sender, flushing every `BATCH_SIZE`.
/// Each received message keeps its identity frame, so sending it back routes
/// it to the peer it came from.
async fn echo(mut router: RouterSocket, total: usize) {
    for echoed in 1..=total {
        let Ok(msg) = router.recv_owned().await else {
            break;
        };
        router.send(msg).await.ok();
        if echoed.is_multiple_of(BATCH_SIZE) {
            router.flush().await.ok();
        }
    }
    router.flush().await.ok();
}

/// Send `MESSAGES_PER_THREAD` messages in batches, receiving each batch's
/// echo before the next.
async fn dealer_rounds(dealer: &mut DealerSocket, payload: &Msg) {
    for _ in 0..(MESSAGES_PER_THREAD / BATCH_SIZE) {
        for _ in 0..BATCH_SIZE {
            dealer.send(black_box(payload.clone())).await.unwrap();
        }
        dealer.flush().await.unwrap();
        for _ in 0..BATCH_SIZE {
            if dealer.recv().await.is_err() {
                break;
            }
        }
    }
}

/// Benchmark multi-threaded DEALER clients against single ROUTER server
///
/// This tests horizontal scalability and lock-free architecture.
#[allow(dead_code)]
fn monocoque_multithreaded_dealers(c: &mut Criterion) {
    let mut group = c.benchmark_group(format!("multithreaded/monocoque-{BENCH_BACKEND}/dealers"));
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10); // Minimum required by criterion

    let payload = Msg::from(vec![0u8; MESSAGE_SIZE]);

    for &num_threads in THREAD_COUNTS {
        let total_messages = num_threads * MESSAGES_PER_THREAD;
        group.throughput(Throughput::Elements(total_messages as u64));

        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    // Use a single runtime for the server
                    let rt = monocoque::rt::LocalRuntime::new().unwrap();

                    rt.block_on(async {
                        // One ROUTER serves every DEALER connection.
                        let mut router = RouterSocket::with_options(coalescing());
                        let endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap();
                        let router_task = monocoque::rt::spawn(echo(router, total_messages));

                        // Spawn N dealer threads, each with its own runtime
                        let mut dealer_handles = Vec::new();
                        for _i in 0..num_threads {
                            let payload = payload.clone();
                            let endpoint = endpoint.clone();

                            let handle = std::thread::spawn(move || {
                                let rt = monocoque::rt::LocalRuntime::new().unwrap();
                                rt.block_on(async {
                                    let mut dealer = DealerSocket::with_options(coalescing());
                                    dealer.connect(&endpoint).await.unwrap();
                                    dealer_rounds(&mut dealer, &payload).await;
                                });
                            });
                            dealer_handles.push(handle);
                        }

                        // The router task runs on this thread, so let it finish
                        // before blocking on the dealer threads.
                        monocoque::rt::join(router_task).await;
                        for handle in dealer_handles {
                            handle.join().unwrap();
                        }
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
    let mut group = c.benchmark_group(format!(
        "multithreaded/monocoque-{BENCH_BACKEND}/independent_pairs"
    ));
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    let payload = Msg::from(vec![0u8; MESSAGE_SIZE]);

    for &num_threads in THREAD_COUNTS {
        let total_messages = num_threads * MESSAGES_PER_THREAD;
        group.throughput(Throughput::Elements(total_messages as u64));

        group.bench_with_input(
            BenchmarkId::new("pairs", num_threads),
            &num_threads,
            |b, &num_threads| {
                b.iter_custom(|iters| {
                    // Each pair sets up ONE persistent DEALER<->ROUTER connection
                    // outside the timed section, then times `iters` rounds of the
                    // batched message workload over that same connection. Reusing
                    // the connection is what makes this measure message throughput
                    // rather than TCP setup, and it avoids churning a fresh
                    // connection (and its TIME_WAIT) per iteration - which, once
                    // the workload is fast, exhausts the ephemeral port range and
                    // fails a later bind with AddrInUse.
                    let mut handles = Vec::new();

                    for _i in 0..num_threads {
                        let payload = payload.clone();

                        let handle = std::thread::spawn(move || {
                            let rt = monocoque::rt::LocalRuntime::new().unwrap();
                            rt.block_on(async {
                                // ROUTER echoes every message across all `iters`
                                // rounds over the single connection.
                                let total = usize::try_from(iters)
                                    .expect("iteration count fits usize")
                                    * MESSAGES_PER_THREAD;
                                let mut router = RouterSocket::with_options(coalescing());
                                let endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap();
                                let router_task = monocoque::rt::spawn(echo(router, total));

                                let mut dealer = DealerSocket::with_options(coalescing());
                                dealer.connect(&endpoint).await.unwrap();
                                wait_connected(&dealer).await;

                                // Timed: `iters` rounds over the one connection.
                                let start = std::time::Instant::now();
                                for _ in 0..iters {
                                    dealer_rounds(&mut dealer, &payload).await;
                                }
                                let elapsed = start.elapsed();

                                monocoque::rt::join(router_task).await;
                                drop(dealer);
                                elapsed
                            })
                        });
                        handles.push(handle);
                    }

                    // Pairs run concurrently; the batch wall-clock is the slowest.
                    let mut max_elapsed = Duration::ZERO;
                    for handle in handles {
                        let e = handle.join().unwrap();
                        if e > max_elapsed {
                            max_elapsed = e;
                        }
                    }
                    max_elapsed
                });
            },
        );
    }
    group.finish();
}

/// Benchmark CPU core utilization efficiency
///
/// Measures how efficiently threads utilize CPU cores (msg/sec per core).
#[allow(dead_code)]
fn monocoque_core_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group(format!(
        "multithreaded/monocoque-{BENCH_BACKEND}/core_efficiency"
    ));
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    let payload = Msg::from(vec![0u8; MESSAGE_SIZE]);
    let num_cores = num_cpus::get();

    // Test at 50%, 100%, and 150% of available cores
    let test_counts = vec![num_cores / 2, num_cores, num_cores + num_cores / 2];

    for num_threads in test_counts {
        if num_threads == 0 {
            continue;
        }

        let total_messages = num_threads * MESSAGES_PER_THREAD;
        group.throughput(Throughput::Elements(total_messages as u64));

        group.bench_with_input(
            BenchmarkId::new("cores", format!("{num_threads}/{num_cores}")),
            &num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let mut handles = Vec::new();

                    for _i in 0..num_threads {
                        let payload = payload.clone();

                        let handle = std::thread::spawn(move || {
                            let rt = monocoque::rt::LocalRuntime::new().unwrap();
                            rt.block_on(async {
                                let mut router = RouterSocket::with_options(coalescing());
                                let endpoint = router.bind("tcp://127.0.0.1:0").await.unwrap();
                                let router_task =
                                    monocoque::rt::spawn(echo(router, MESSAGES_PER_THREAD));

                                let mut dealer = DealerSocket::with_options(coalescing());
                                dealer.connect(&endpoint).await.unwrap();
                                dealer_rounds(&mut dealer, &payload).await;

                                monocoque::rt::join(router_task).await;
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
