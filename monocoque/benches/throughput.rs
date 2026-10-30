//! Throughput benchmarks: messages per second (PUSH/PULL one-way)
//!
//! Compares monocoque vs rust-zmq (zmq crate, FFI bindings to libzmq) for raw throughput.
//! Measures how many messages can be delivered per second in a PUSH->PULL pipeline.
//!
//! ## Methodology
//!
//! - Sender and receiver run on separate OS threads, each with their own runtime.
//! - The PULL side receives one warmup message first, so the timer starts on a
//!   live connection for both monocoque and libzmq.
//! - Both monocoque and zmq use the same protocol: one send per message, no reply.
//! - `push_pull_coalesced` enables write coalescing (64 KB flush threshold) to
//!   batch multiple sends into a single kernel write, closing the gap with
//!   libzmq's internal IO-thread batching.
//! - `push_pull_coalesced_owned` receives with `recv_owned`, so comparing it with
//!   `push_pull_coalesced` (borrowed `recv`) isolates the cost of taking ownership.

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
use std::thread;
use std::time::{Duration, Instant};

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024, 4096, 16384];
const BATCH_SIZE: usize = 10_000;

#[derive(Clone, Copy)]
enum Mode {
    Eager,
    Coalesced,
    CoalescedOwned,
}

/// One timed PUSH/PULL run. PULL binds on its own OS thread and times
/// `BATCH_SIZE` receives after a warmup message; PUSH sends `BATCH_SIZE + 1`.
fn run_once(payload: &Msg, mode: Mode) -> Duration {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

    let pull_thread = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut pull =
                PullSocket::with_options(SocketOptions::default().with_read_buffer_size(16384));
            ep_tx
                .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            pull.recv().await.unwrap();
            let t0 = Instant::now();
            if matches!(mode, Mode::CoalescedOwned) {
                for _ in 0..BATCH_SIZE {
                    std::hint::black_box(pull.recv_owned().await.unwrap());
                }
            } else {
                for _ in 0..BATCH_SIZE {
                    std::hint::black_box(pull.recv().await.unwrap());
                }
            }
            elapsed_tx.send(t0.elapsed()).unwrap();
        });
    });

    let endpoint = ep_rx.recv().unwrap();
    let payload = payload.clone();
    let push_rt = monocoque::rt::LocalRuntime::new().unwrap();
    let elapsed = push_rt.block_on(async move {
        let mut options = SocketOptions::default().with_read_buffer_size(16384);
        if !matches!(mode, Mode::Eager) {
            options = options.with_write_coalescing(true);
        }
        let mut push = PushSocket::with_options(options);
        push.connect(&endpoint).await.unwrap();
        for _ in 0..=BATCH_SIZE {
            push.send(payload.clone()).await.unwrap();
        }
        // Release bytes that did not reach the coalescing threshold.
        push.flush().await.unwrap();
        // Keep the connection up until the receiver has everything.
        let elapsed = elapsed_rx.recv().unwrap();
        let _ = push.close().await;
        elapsed
    });
    pull_thread.join().unwrap();
    elapsed
}

fn monocoque_group(c: &mut Criterion, name: &str, mode: Mode) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("throughput/monocoque-{BENCH_BACKEND}/{name}"));
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Msg::from(vec![0u8; size]);
            b.iter_custom(|iters| (0..iters).map(|_| run_once(&payload, mode)).sum());
        });
    }

    group.finish();
}

/// Benchmark monocoque PUSH/PULL throughput - eager (one write per message).
fn monocoque_push_pull(c: &mut Criterion) {
    monocoque_group(c, "push_pull", Mode::Eager);
}

/// Benchmark monocoque PUSH/PULL throughput - with write coalescing enabled.
fn monocoque_push_pull_coalesced(c: &mut Criterion) {
    monocoque_group(c, "push_pull_coalesced", Mode::Coalesced);
}

/// Benchmark monocoque PUSH/PULL throughput - coalesced, PULL taking ownership.
fn monocoque_push_pull_coalesced_owned(c: &mut Criterion) {
    monocoque_group(c, "push_pull_coalesced_owned", Mode::CoalescedOwned);
}

/// Benchmark rust-zmq (libzmq) PUSH/PULL throughput.
///
/// Same structure as the monocoque path: PULL binds in a separate thread,
/// PUSH connects in the bench thread. Timer on the PULL side.
fn zmq_push_pull(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput/zmq/push_pull");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = vec![0u8; size];

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (endpoint_tx, endpoint_rx) = mpsc::channel::<String>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

                    let pull_thread = thread::spawn(move || {
                        let ctx = zmq::Context::new();
                        let pull = ctx.socket(zmq::PULL).unwrap();
                        pull.bind("tcp://127.0.0.1:*").unwrap();
                        let endpoint = pull.get_last_endpoint().unwrap().unwrap();
                        endpoint_tx.send(endpoint).unwrap();

                        // Warmup message: the timer starts on a live connection.
                        pull.recv_bytes(0).unwrap();
                        let t0 = Instant::now();
                        for _ in 0..BATCH_SIZE {
                            pull.recv_bytes(0).unwrap();
                        }
                        elapsed_tx.send(t0.elapsed()).unwrap();
                    });

                    let endpoint = endpoint_rx.recv().unwrap();

                    let ctx = zmq::Context::new();
                    let push = ctx.socket(zmq::PUSH).unwrap();
                    push.connect(&endpoint).unwrap();

                    for _ in 0..=BATCH_SIZE {
                        push.send(&payload, 0).unwrap();
                    }

                    pull_thread.join().unwrap();
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
        .measurement_time(Duration::from_mins(1))
        .warm_up_time(Duration::from_secs(5))
        .sample_size(10);
    targets =
        monocoque_push_pull,
        monocoque_push_pull_coalesced,
        monocoque_push_pull_coalesced_owned,
        zmq_push_pull
);
criterion_main!(benches);
