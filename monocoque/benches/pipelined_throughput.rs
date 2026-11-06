//! DEALER/ROUTER pipelined throughput with write coalescing.
//!
//! Both sides enable write coalescing, issue a batch of `send()` calls and
//! then one `flush()`, so each batch goes out in one kernel write. This
//! measures that coalesced batch pattern; compare it with the one-send-per-
//! message benches.
//! It is NOT a cross-library comparison -- use throughput.rs for that.
//!
//! ## Pattern
//!
//! Process messages in batches to avoid TCP backpressure:
//! 1. DEALER: coalesced `send` batch -> flush
//! 2. ROUTER: receive batch -> coalesced `send` batch -> flush
//! 3. DEALER: receive batch
//! 4. Repeat
//!
//! DEALER and ROUTER run on separate OS threads (each with its own
//! runtime) so the measured time reflects real inter-thread communication.

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
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024, 4096, 16384];
const TOTAL_MESSAGES: usize = 10_000;
const BATCH_SIZE: usize = 100;

/// Write coalescing holds every send until the explicit `flush`, which is
/// what batches the writes: one kernel write per batch instead of one per message.
fn batch_options(options: SocketOptions) -> SocketOptions {
    options.with_write_coalescing(true)
}

/// Benchmark monocoque DEALER/ROUTER pipelined throughput with coalesced batches.
///
/// ROUTER binds in a separate OS thread. DEALER connects in the bench thread.
/// Both use coalesced sends plus `flush` to minimize syscall overhead.
fn monocoque_dealer_router_pipelined(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("pipelined/monocoque-{BENCH_BACKEND}/dealer_router"));
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        let payload = Msg::from(vec![0u8; size]);

        group.throughput(Throughput::Bytes((size * TOTAL_MESSAGES) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (ep_tx, ep_rx) = mpsc::channel::<String>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();
                    let payload_clone = payload.clone();

                    // ROUTER thread: bind, receive + echo in batches, report elapsed.
                    let router_thread = thread::spawn(move || {
                        let rt = monocoque::rt::LocalRuntime::new().unwrap();
                        rt.block_on(async move {
                            let mut router =
                                RouterSocket::with_options(batch_options(SocketOptions::default()));
                            ep_tx
                                .send(router.bind("tcp://127.0.0.1:0").await.unwrap())
                                .unwrap();

                            // Start timing once the DEALER is handshaken.
                            while router.peer_count() == 0 {
                                monocoque::rt::sleep(Duration::from_millis(1)).await;
                            }

                            let t0 = std::time::Instant::now();
                            let mut batch = Vec::with_capacity(BATCH_SIZE);
                            for _ in 0..(TOTAL_MESSAGES / BATCH_SIZE) {
                                for _ in 0..BATCH_SIZE {
                                    if let Ok(msg) = router.recv_owned().await {
                                        batch.push(msg);
                                    }
                                }
                                // Each message still carries its identity frame,
                                // so sending it back routes it to the DEALER.
                                // `drain` keeps the batch's capacity for reuse.
                                #[allow(clippy::iter_with_drain)]
                                for msg in batch.drain(..) {
                                    router.send(msg).await.unwrap();
                                }
                                router.flush().await.unwrap();
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                            let _ = router.close().await;
                        });
                    });

                    let endpoint = ep_rx.recv().unwrap();

                    // DEALER thread: connect, send + recv in batches.
                    let dealer_rt = monocoque::rt::LocalRuntime::new().unwrap();
                    let elapsed = dealer_rt.block_on(async move {
                        let mut dealer = DealerSocket::with_options(batch_options(
                            SocketOptions::default().with_read_buffer_size(16384),
                        ));
                        dealer.connect(&endpoint).await.unwrap();

                        for _ in 0..(TOTAL_MESSAGES / BATCH_SIZE) {
                            for _ in 0..BATCH_SIZE {
                                dealer.send(black_box(payload_clone.clone())).await.unwrap();
                            }
                            dealer.flush().await.unwrap();

                            // Borrowed receive: the echoed batch is drained
                            // without allocating.
                            for _ in 0..BATCH_SIZE {
                                if dealer.recv().await.is_err() {
                                    break;
                                }
                            }
                        }
                        // Keep the connection up until the ROUTER has finished.
                        let elapsed = elapsed_rx.recv().unwrap();
                        let _ = dealer.close().await;
                        elapsed
                    });

                    router_thread.join().unwrap();
                    total += elapsed;
                }

                total
            });
        });
    }
    group.finish();
}

criterion_group!(benches, monocoque_dealer_router_pipelined);
criterion_main!(benches);
