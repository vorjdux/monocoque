//! PUB fan-out with one deliberately slow subscriber.
//!
//! One PUB serves 100 SUB peers. 99 drain at full speed; one reads far slower
//! than the publish rate. The benchmark measures the delivered throughput to the
//! FAST subscribers while the slow one lags, i.e. whether a slow consumer drags
//! down delivery to the healthy majority.
//!
//! ## Modeling limitation
//!
//! A truly non-draining subscriber cannot be modeled as "never call recv": once
//! its per-peer send buffer / HWM fills, the PUB drops for that peer (PUB/SUB is
//! lossy by contract), so the slow peer stops exerting backpressure and the test
//! degenerates. Instead the slow subscriber reads on a fixed slow cadence (a
//! sleep between recvs), so it stays continuously far behind the publish rate -
//! the realistic "slow consumer" shape. This is the documented approximation the
//! deliverable calls for.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use monocoque::zmq::{Msg, PubSocket, SocketOptions, SubSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// Identifies which runtime backend this build benchmarks, so compio, tokio, and
// smol results land under distinct criterion ids instead of overwriting.
const BENCH_BACKEND: &str = if cfg!(feature = "runtime-tokio") {
    "tokio"
} else if cfg!(feature = "runtime-smol") {
    "smol"
} else {
    "compio"
};

const TOTAL_SUBS: usize = 100;
const FAST_SUBS: usize = TOTAL_SUBS - 1;
const MESSAGE_SIZE: usize = 64;
/// Messages each fast subscriber counts per criterion iteration.
const MESSAGE_COUNT: usize = 100;
/// Untimed warmup receives to confirm the stream is flowing and prime the pipe.
const WARMUP_MSGS: usize = 100;
/// The slow subscriber reads one message per this interval, staying far behind.
const SLOW_INTERVAL: Duration = Duration::from_millis(5);
/// Brief settle after subscriptions are issued, cutting the slow-joiner drop
/// burst.
const SETTLE: Duration = Duration::from_millis(50);
/// Longest the publisher waits for queued writes when a subscriber is full.
const FLUSH_SLICE: Duration = Duration::from_millis(1);

fn sub_options() -> SocketOptions {
    SocketOptions::default().with_read_buffer_size(16384)
}

/// Receive exactly `n` messages, returning early only on an error.
async fn recv_n(sub: &mut SubSocket, n: usize) {
    for _ in 0..n {
        if sub.recv().await.is_err() {
            return;
        }
    }
}

/// Time delivery of `iters * MESSAGE_COUNT` messages to each FAST subscriber
/// while one slow subscriber lags and the publisher oversends. Returns the
/// slowest fast subscriber's elapsed time.
fn run_fanout_slow(iters: u64) -> Duration {
    let target = usize::try_from(iters).expect("iteration count fits usize") * MESSAGE_COUNT;
    let payload = Msg::from(vec![0u8; MESSAGE_SIZE]);

    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    // Publisher: wait for every subscriber, then oversend until told to stop.
    let pub_handle = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut pub_socket = PubSocket::new();
            ep_tx
                .send(pub_socket.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            while pub_socket.send(payload.clone()).await.unwrap_or(0) < TOTAL_SUBS {
                monocoque::rt::sleep(Duration::from_millis(1)).await;
            }
            thread::sleep(SETTLE);
            while stop_rx.try_recv().is_err() {
                // PUB never waits on a send. When a queue is full (the slow
                // subscriber's, at least), hand the runtime a turn to write,
                // bounded so the slow subscriber cannot hold the publisher
                // back for longer than FLUSH_SLICE.
                if pub_socket.send(payload.clone()).await.unwrap_or(0) < TOTAL_SUBS {
                    let _ = monocoque::rt::timeout(FLUSH_SLICE, pub_socket.flush()).await;
                }
            }
        });
    });

    let endpoint = ep_rx.recv().unwrap();

    // One slow subscriber: reads on a fixed slow cadence, never keeping up.
    let slow_endpoint = endpoint.clone();
    let (slow_stop_tx, slow_stop_rx) = mpsc::channel::<()>();
    let slow_handle = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut sub = SubSocket::with_options(
                sub_options().with_recv_timeout(Duration::from_millis(100)),
            );
            sub.subscribe(b"").await.unwrap();
            sub.connect(&slow_endpoint).await.unwrap();
            // Read slowly until the measurement is over. The receive timeout
            // lets it notice the stop once the publisher has gone quiet.
            while slow_stop_rx.try_recv().is_err() {
                thread::sleep(SLOW_INTERVAL);
                let _ = sub.recv().await;
            }
        });
    });

    // Fast subscribers: drain at full speed and time their window.
    let mut fast_handles = Vec::with_capacity(FAST_SUBS);
    for _ in 0..FAST_SUBS {
        let endpoint = endpoint.clone();
        fast_handles.push(thread::spawn(move || {
            let rt = monocoque::rt::LocalRuntime::new().unwrap();
            rt.block_on(async move {
                let mut sub = SubSocket::with_options(sub_options());
                sub.subscribe(b"").await.unwrap();
                sub.connect(&endpoint).await.unwrap();
                recv_n(&mut sub, WARMUP_MSGS).await; // untimed warmup
                let start = Instant::now();
                recv_n(&mut sub, target).await;
                start.elapsed()
            })
        }));
    }

    // Slowest fast subscriber bounds delivered fan-out throughput.
    let elapsed = fast_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .max()
        .unwrap_or_default();

    let _ = stop_tx.send(());
    pub_handle.join().unwrap();
    let _ = slow_stop_tx.send(());
    slow_handle.join().unwrap();
    elapsed
}

fn fanout_slow_subscriber(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("fanout_slow_subscriber/monocoque-{BENCH_BACKEND}"));
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(10);
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));
    group.bench_function("fast_delivery_with_1_slow_of_100", |b| {
        b.iter_custom(run_fanout_slow);
    });
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5))
        .sample_size(10);
    targets = fanout_slow_subscriber
);
criterion_main!(benches);
