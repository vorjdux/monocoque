//! Throughput benchmarks for the fan-out and fan-in worker-pool topologies.
//!
//! These cover ground the single-connection PUSH/PULL benches cannot: one
//! ventilator spreading work across a pool of workers, and one sink merging a
//! pool of senders. The ventilator is one PUSH socket with `WORKERS` peers and
//! the sink is one PULL socket with `WORKERS` peers.
//!
//! ## Methodology
//!
//! - Every socket runs on its own OS thread with its own runtime.
//! - `BATCH_SIZE` messages cross the pool per iteration, split evenly across
//!   `WORKERS` connections.
//! - Connection setup and the ZMTP handshake happen outside the timed window.
//! - Senders use write coalescing. Coalesced writes are held until a flush, so
//!   senders flush every `COALESCE_BATCH` messages (well under the default
//!   send HWM of 1000 per peer) and once at the end.
//!
//! Fan-out has N parallel receivers, so each worker times its own receive window
//! from a shared start barrier and the iteration cost is the slowest worker's
//! window (the point at which the whole batch has landed). Fan-in has a single
//! sink, so the timer lives on the sink side exactly like the PUSH/PULL
//! throughput bench.

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
use monocoque::zmq::{Msg, PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const MESSAGE_SIZES: &[usize] = &[64, 1024, 16384];
const BATCH_SIZE: usize = 10_000;
const WORKERS: usize = 4;
const PER_WORKER: usize = BATCH_SIZE / WORKERS;
/// Messages per peer between flushes of a coalescing sender.
const COALESCE_BATCH: usize = 256;

fn coalescing_options() -> SocketOptions {
    SocketOptions::default()
        .with_buffer_sizes(16384, 16384)
        .with_write_coalescing(true)
}

/// Wait until `peers` peers are connected and handshaken.
async fn wait_peers(peer_count: impl Fn() -> usize, peers: usize) {
    while peer_count() < peers {
        monocoque::rt::sleep(Duration::from_millis(1)).await;
    }
}

/// Fan-out: one PUSH ventilator round-robins `BATCH_SIZE` messages across
/// `WORKERS` PULL workers.
///
/// The ventilator and all workers meet at a barrier so they start together. Each
/// worker times its own receive window and reports it; the iteration cost is the
/// slowest worker's window, i.e. when the last message of the batch arrives.
fn monocoque_fanout(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("fanout_fanin/monocoque-{BENCH_BACKEND}/fanout"));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Msg::from(vec![0u8; size]);

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (ep_tx, ep_rx) = mpsc::channel::<String>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();
                    // WORKERS receivers plus the ventilator align on this barrier.
                    let barrier = Arc::new(Barrier::new(WORKERS + 1));
                    // Reached once every worker has its share, so the ventilator
                    // keeps its connections up until then.
                    let done = Arc::new(Barrier::new(WORKERS + 1));

                    let vent_payload = payload.clone();
                    let vent_barrier = barrier.clone();
                    let vent_done = done.clone();
                    let ventilator = thread::spawn(move || {
                        let rt = monocoque::rt::LocalRuntime::new().unwrap();
                        rt.block_on(async move {
                            let mut fanout = PushSocket::with_options(coalescing_options());
                            ep_tx
                                .send(fanout.bind("tcp://127.0.0.1:0").await.unwrap())
                                .unwrap();
                            wait_peers(|| fanout.peer_count(), WORKERS).await;

                            // Per-message round-robin with coalescing keeps all
                            // workers interleaved and batches each worker's writes
                            // until the next flush. Batching a whole worker share
                            // instead serializes the pool, so this is the faster
                            // path.
                            vent_barrier.wait();
                            for i in 1..=BATCH_SIZE {
                                fanout.send(vent_payload.clone()).await.unwrap();
                                if i % (COALESCE_BATCH * WORKERS) == 0 {
                                    fanout.flush().await.unwrap();
                                }
                            }
                            fanout.flush().await.unwrap();
                            vent_done.wait();
                            let _ = fanout.close().await;
                        });
                    });

                    let endpoint = ep_rx.recv().unwrap();

                    for _ in 0..WORKERS {
                        let worker_barrier = barrier.clone();
                        let worker_done = done.clone();
                        let worker_elapsed = elapsed_tx.clone();
                        let endpoint = endpoint.clone();
                        thread::spawn(move || {
                            let rt = monocoque::rt::LocalRuntime::new().unwrap();
                            rt.block_on(async move {
                                let mut pull = PullSocket::new();
                                pull.connect(&endpoint).await.unwrap();
                                wait_peers(|| pull.peer_count(), 1).await;
                                worker_barrier.wait();
                                let t0 = Instant::now();
                                for _ in 0..PER_WORKER {
                                    pull.recv().await.unwrap();
                                }
                                worker_elapsed.send(t0.elapsed()).unwrap();
                                worker_done.wait();
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
    fanin(
        c,
        &format!("fanout_fanin/monocoque-{BENCH_BACKEND}/fanin_coalesced"),
        true,
    );
}

/// Fan-in with eager senders: one kernel write per message, so a kernel read on
/// the sink may carry as little as one message. This is the case where batching
/// the merge channel could in principle add overhead rather than amortize it.
fn monocoque_fanin_eager(c: &mut Criterion) {
    fanin(
        c,
        &format!("fanout_fanin/monocoque-{BENCH_BACKEND}/fanin_eager"),
        false,
    );
}

/// Fan-in: `WORKERS` PUSH workers each send `PER_WORKER` messages to one PULL
/// sink.
///
/// The sink is the single receiver, so the timer lives on its side: it starts
/// once every worker is connected and stops after the whole batch is drained.
/// `coalesce` selects whether the senders batch writes or send eagerly.
fn fanin(c: &mut Criterion, group_name: &str, coalesce: bool) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(group_name);
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Msg::from(vec![0u8; size]);

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (ep_tx, ep_rx) = mpsc::channel::<String>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();
                    // Reached once the sink has drained the batch, so no worker
                    // disconnects while its messages are in flight.
                    let done = Arc::new(Barrier::new(WORKERS + 1));

                    let sink_done = done.clone();
                    let sink_thread = thread::spawn(move || {
                        let rt = monocoque::rt::LocalRuntime::new().unwrap();
                        rt.block_on(async move {
                            let mut sink = PullSocket::with_options(
                                SocketOptions::default().with_buffer_sizes(16384, 16384),
                            );
                            ep_tx
                                .send(sink.bind("tcp://127.0.0.1:0").await.unwrap())
                                .unwrap();
                            wait_peers(|| sink.peer_count(), WORKERS).await;

                            let t0 = Instant::now();
                            for _ in 0..BATCH_SIZE {
                                sink.recv().await.unwrap();
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                            sink_done.wait();
                        });
                    });

                    let endpoint = ep_rx.recv().unwrap();

                    let mut workers = Vec::with_capacity(WORKERS);
                    for _ in 0..WORKERS {
                        let worker_payload = payload.clone();
                        let worker_done = done.clone();
                        let endpoint = endpoint.clone();
                        workers.push(thread::spawn(move || {
                            let rt = monocoque::rt::LocalRuntime::new().unwrap();
                            rt.block_on(async move {
                                let options = if coalesce {
                                    coalescing_options()
                                } else {
                                    SocketOptions::default().with_buffer_sizes(16384, 16384)
                                };
                                let mut push = PushSocket::with_options(options);
                                push.connect(&endpoint).await.unwrap();
                                for i in 1..=PER_WORKER {
                                    push.send(worker_payload.clone()).await.unwrap();
                                    if coalesce && i % COALESCE_BATCH == 0 {
                                        push.flush().await.unwrap();
                                    }
                                }
                                // No-op in eager mode; drains the buffer when coalescing.
                                push.flush().await.unwrap();
                                worker_done.wait();
                                let _ = push.close().await;
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
