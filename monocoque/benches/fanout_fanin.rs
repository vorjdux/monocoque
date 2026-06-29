//! Throughput benchmarks for the fan-out and fan-in worker-pool topologies.
//!
//! These cover ground the single-connection PUSH/PULL benches cannot: one
//! ventilator spreading work across a pool of workers, and one sink merging a
//! pool of senders. Both use `PushFanOut` / `PullFanIn`.
//!
//! ## Methodology
//!
//! - Every socket runs on its own OS thread with its own compio runtime.
//! - `BATCH_SIZE` messages cross the pool per iteration, split evenly across
//!   `WORKERS` connections.
//! - Connection setup and the ZMTP handshake happen outside the timed window.
//! - Senders use write coalescing with a final flush, matching the maximum
//!   throughput path used by the cross-implementation bench peer.
//!
//! Fan-out has N parallel receivers, so each worker times its own receive window
//! from a shared start barrier and the iteration cost is the slowest worker's
//! window (the point at which the whole batch has landed). Fan-in has a single
//! sink, so the timer lives on the sink side exactly like the PUSH/PULL
//! throughput bench.

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use monocoque::zmq::{PullFanIn, PullSocket, PushFanOut, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const MESSAGE_SIZES: &[usize] = &[64, 1024, 16384];
const BATCH_SIZE: usize = 10_000;
const WORKERS: usize = 4;
const PER_WORKER: usize = BATCH_SIZE / WORKERS;

fn coalescing_options() -> SocketOptions {
    SocketOptions::default()
        .with_buffer_sizes(16384, 16384)
        .with_write_coalescing(true)
}

/// Fan-out: one `PushFanOut` ventilator round-robins `BATCH_SIZE` messages across
/// `WORKERS` PULL workers.
///
/// The ventilator and all workers meet at a barrier so they start together. Each
/// worker times its own receive window and reports it; the iteration cost is the
/// slowest worker's window, i.e. when the last message of the batch arrives.
fn monocoque_fanout(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("fanout_fanin/monocoque/fanout");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Bytes::from(vec![0u8; size]);

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (port_tx, port_rx) = mpsc::channel::<u16>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();
                    // WORKERS receivers plus the ventilator align on this barrier.
                    let barrier = Arc::new(Barrier::new(WORKERS + 1));

                    let vent_payload = payload.clone();
                    let vent_barrier = barrier.clone();
                    let ventilator = thread::spawn(move || {
                        let rt = compio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            port_tx.send(listener.local_addr().unwrap().port()).unwrap();

                            let mut fanout = PushFanOut::accept_workers(
                                &listener,
                                WORKERS,
                                coalescing_options(),
                            )
                            .await
                            .unwrap();

                            // Per-message round-robin with coalescing keeps all
                            // workers interleaved and batches each worker's writes
                            // at the 64 KB coalesce threshold. Batching a whole
                            // worker share instead serializes the pool, so this is
                            // the faster path.
                            vent_barrier.wait();
                            for _ in 0..BATCH_SIZE {
                                fanout.send(vec![vent_payload.clone()]).await.unwrap();
                            }
                            fanout.flush().await.unwrap();
                        });
                    });

                    let port = port_rx.recv().unwrap();

                    for _ in 0..WORKERS {
                        let worker_barrier = barrier.clone();
                        let worker_elapsed = elapsed_tx.clone();
                        thread::spawn(move || {
                            let rt = compio::runtime::Runtime::new().unwrap();
                            rt.block_on(async move {
                                let mut pull =
                                    PullSocket::connect(("127.0.0.1", port)).await.unwrap();
                                worker_barrier.wait();
                                let t0 = Instant::now();
                                for _ in 0..PER_WORKER {
                                    pull.recv().await.unwrap();
                                }
                                worker_elapsed.send(t0.elapsed()).unwrap();
                            });
                        });
                    }
                    drop(elapsed_tx);

                    let mut slowest = Duration::ZERO;
                    for _ in 0..WORKERS {
                        slowest = slowest.max(elapsed_rx.recv().unwrap());
                    }
                    ventilator.join().unwrap();
                    total += slowest;
                }

                total
            });
        });
    }

    group.finish();
}

/// Fan-in with write-coalescing senders: many messages per kernel write, so each
/// kernel read on the sink carries a big batch.
fn monocoque_fanin_coalesced(c: &mut Criterion) {
    fanin(c, "fanout_fanin/monocoque/fanin_coalesced", true);
}

/// Fan-in with eager senders: one kernel write per message, so a kernel read on
/// the sink may carry as little as one message. This is the case where batching
/// the merge channel could in principle add overhead rather than amortize it.
fn monocoque_fanin_eager(c: &mut Criterion) {
    fanin(c, "fanout_fanin/monocoque/fanin_eager", false);
}

/// Fan-in: `WORKERS` PUSH workers each send `PER_WORKER` messages to one
/// `PullFanIn` sink.
///
/// The sink is the single receiver, so the timer lives on its side: it starts
/// just before the first merged recv and stops after the whole batch is drained.
/// `coalesce` selects whether the senders batch writes or send eagerly.
fn fanin(c: &mut Criterion, group_name: &str, coalesce: bool) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(group_name);
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Bytes::from(vec![0u8; size]);

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (port_tx, port_rx) = mpsc::channel::<u16>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

                    let sink_thread = thread::spawn(move || {
                        let rt = compio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            port_tx.send(listener.local_addr().unwrap().port()).unwrap();

                            let mut sink = PullFanIn::accept_workers(
                                &listener,
                                WORKERS,
                                SocketOptions::default().with_buffer_sizes(16384, 16384),
                            )
                            .await
                            .unwrap();

                            let t0 = Instant::now();
                            // One await returns a whole burst of merged messages.
                            let mut received = 0usize;
                            while received < BATCH_SIZE {
                                match sink.recv_batch().await.unwrap() {
                                    Some(batch) => received += batch.len(),
                                    None => break,
                                }
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                        });
                    });

                    let port = port_rx.recv().unwrap();

                    let mut workers = Vec::with_capacity(WORKERS);
                    for _ in 0..WORKERS {
                        let worker_payload = payload.clone();
                        workers.push(thread::spawn(move || {
                            let rt = compio::runtime::Runtime::new().unwrap();
                            rt.block_on(async move {
                                let options = if coalesce {
                                    coalescing_options()
                                } else {
                                    SocketOptions::default().with_buffer_sizes(16384, 16384)
                                };
                                let mut push =
                                    PushSocket::connect_with_options(("127.0.0.1", port), options)
                                        .await
                                        .unwrap();
                                for _ in 0..PER_WORKER {
                                    push.send(vec![worker_payload.clone()]).await.unwrap();
                                }
                                // No-op in eager mode; drains the buffer when coalescing.
                                push.flush().await.unwrap();
                            });
                        }));
                    }

                    for worker in workers {
                        worker.join().unwrap();
                    }
                    sink_thread.join().unwrap();
                    total += elapsed_rx.recv().unwrap();
                }

                total
            });
        });
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(20))
        .warm_up_time(Duration::from_secs(5))
        .sample_size(10);
    targets =
        monocoque_fanout,
        monocoque_fanin_coalesced,
        monocoque_fanin_eager
);
criterion_main!(benches);
