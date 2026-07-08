//! Pattern-specific benchmarks: PUB/SUB fanout and topic filtering.
//!
//! Compares monocoque vs rust-zmq (zmq crate) for PUB/SUB messaging patterns.
//! Tests the PUBLIC API from `monocoque::zmq`.
//!
//! ## Timing methodology
//!
//! Connection setup (bind, accept, connect, subscribe) happens **once per
//! `iter_custom` call, outside the timed region**. The publisher then *oversends*
//! continuously while the subscriber times how long it takes to receive a fixed
//! number of messages. This measures steady-state **delivered** throughput and
//! is robust by construction:
//!
//! - PUB/SUB is lossy (the publisher drops on HWM and during the slow-joiner
//!   window before a subscription propagates). Because the publisher keeps
//!   producing, the subscriber always reaches its target count - a dropped
//!   message just means the next delivered one arrives slightly later. There is
//!   no exact-count receive that can block forever on a lost frame, which is
//!   what made the previous version hang on the rust-zmq slow joiner.
//! - A warmup receive (untimed) confirms the pipeline is flowing and primes it
//!   before the clock starts.

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

// Identifies which runtime backend this build benchmarks, so compio, tokio, and smol
// results land under distinct criterion ids instead of overwriting each other.
const BENCH_BACKEND: &str = if cfg!(feature = "runtime-tokio") {
    "tokio"
} else if cfg!(feature = "runtime-smol") {
    "smol"
} else {
    "compio"
};
use monocoque::zmq::{PubSocket, SocketOptions, SubSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// NOTE: Multi-subscriber fanout is currently not benchmarked here because the
// direct-stream PUB/SUB sockets do not yet support stable multi-peer fanout.
const FANOUT_SUBSCRIBERS: &[usize] = &[1];
const MESSAGE_COUNT: usize = 100; // elements counted per criterion iteration
const MESSAGE_SIZE: usize = 256;
/// Messages received (untimed) to confirm the stream is flowing and warm the
/// pipeline before timing starts.
const WARMUP_MSGS: usize = 200;
/// Brief settle after the subscription is issued, to cut the initial
/// slow-joiner drop burst (untimed). Uses `thread::sleep`, not
/// `monocoque::rt::sleep`, which would block on the stalled io_uring handshake
/// timer left behind by `accept_subscriber`.
const SETTLE: Duration = Duration::from_millis(50);
/// Of every `MESSAGE_COUNT` published in the topic-filtering test, this many
/// match the subscription.
const MATCHED_PER_ROUND: usize = MESSAGE_COUNT / 10;

// ─────────────────────────────────────────────────────────────────────────────
// monocoque PUB/SUB fanout
// ─────────────────────────────────────────────────────────────────────────────

/// Time the delivery of `iters * MESSAGE_COUNT` messages to each subscriber
/// while the publisher oversends. Returns the slowest subscriber's elapsed time.
fn run_monocoque_fanout(num_subs: usize, iters: u64) -> Duration {
    let target = iters as usize * MESSAGE_COUNT;
    let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let pub_handle = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(pub_socket.local_addr().unwrap()).unwrap();
            for _ in 0..num_subs {
                pub_socket.accept_subscriber().await.unwrap();
            }
            thread::sleep(SETTLE);
            // Oversend until every subscriber has measured its window.
            while stop_rx.try_recv().is_err() {
                pub_socket
                    .send_frames(std::slice::from_ref(&payload))
                    .await
                    .ok();
            }
        });
    });

    let server_addr = addr_rx.recv().unwrap();

    let mut sub_handles = Vec::new();
    for _ in 0..num_subs {
        sub_handles.push(thread::spawn(move || {
            let rt = monocoque::rt::LocalRuntime::new().unwrap();
            rt.block_on(async move {
                let stream = monocoque::rt::TcpStream::connect(server_addr)
                    .await
                    .unwrap();
                let mut sub = SubSocket::from_tcp_with_options(
                    stream,
                    SocketOptions::default().with_buffer_sizes(16384, 16384),
                )
                .await
                .unwrap();
                sub.subscribe(b"").await.unwrap();
                recv_n(&mut sub, WARMUP_MSGS).await; // untimed warmup
                let start = Instant::now();
                recv_n(&mut sub, target).await;
                start.elapsed()
            })
        }));
    }

    // Slowest subscriber bounds fanout throughput.
    let elapsed = sub_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .max()
        .unwrap_or_default();

    let _ = stop_tx.send(());
    pub_handle.join().unwrap();
    elapsed
}

/// Receive exactly `n` messages, returning early only on disconnect.
async fn recv_n(sub: &mut SubSocket, n: usize) {
    let mut count = 0;
    while count < n {
        match sub.recv().await {
            Ok(Some(_)) => count += 1,
            Ok(None) | Err(_) => return,
        }
    }
}

fn monocoque_pubsub_fanout(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("patterns/monocoque-{BENCH_BACKEND}/pubsub_fanout"));
    for &num_subs in FANOUT_SUBSCRIBERS {
        group.throughput(Throughput::Elements((MESSAGE_COUNT * num_subs) as u64));
        group.bench_with_input(
            BenchmarkId::new("subscribers", num_subs),
            &num_subs,
            |b, &num_subs| {
                b.iter_custom(|iters| run_monocoque_fanout(num_subs, iters));
            },
        );
    }
    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// rust-zmq PUB/SUB fanout
// ─────────────────────────────────────────────────────────────────────────────

fn run_zmq_fanout(num_subs: usize, iters: u64) -> Duration {
    let target = iters as usize * MESSAGE_COUNT;
    let payload = vec![0u8; MESSAGE_SIZE];

    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (synced_tx, synced_rx) = mpsc::channel::<()>(); // sub -> pub: "receiving"

    let pub_handle = thread::spawn(move || {
        let ctx = zmq::Context::new();
        let pub_socket = ctx.socket(zmq::PUB).unwrap();
        pub_socket.set_linger(0).unwrap();
        pub_socket.bind("tcp://127.0.0.1:*").unwrap();
        let endpoint = pub_socket.get_last_endpoint().unwrap().unwrap();
        for _ in 0..num_subs {
            ep_tx.send(endpoint.clone()).unwrap();
        }
        // Sync phase: send in small bursts with a yield between them so zmq's
        // background I/O thread can register the subscription, until every
        // subscriber confirms it is receiving. Without this, the full-speed
        // loop below starves subscription processing and the slow joiner never
        // gets a single message.
        let mut synced = 0;
        while synced < num_subs {
            for _ in 0..16 {
                pub_socket.send(&payload, 0).unwrap();
            }
            thread::sleep(Duration::from_millis(1));
            while synced_rx.try_recv().is_ok() {
                synced += 1;
            }
        }
        // Timed phase: oversend at full speed (subscription is now live).
        while stop_rx.try_recv().is_err() {
            pub_socket.send(black_box(&payload), 0).unwrap();
        }
    });

    let mut sub_handles = Vec::new();
    for _ in 0..num_subs {
        let endpoint = ep_rx.recv().unwrap();
        let synced_tx = synced_tx.clone();
        sub_handles.push(thread::spawn(move || {
            let ctx = zmq::Context::new();
            let sub = ctx.socket(zmq::SUB).unwrap();
            sub.set_linger(0).unwrap();
            sub.connect(&endpoint).unwrap();
            sub.set_subscribe(b"").unwrap();
            let _ = sub.recv_bytes(0); // block until the subscription is live
            synced_tx.send(()).unwrap();
            for _ in 0..WARMUP_MSGS {
                let _ = sub.recv_bytes(0);
            }
            let start = Instant::now();
            let mut count = 0;
            while count < target {
                if sub.recv_bytes(0).is_ok() {
                    count += 1;
                }
            }
            start.elapsed()
        }));
    }

    let elapsed = sub_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .max()
        .unwrap_or_default();

    let _ = stop_tx.send(());
    pub_handle.join().unwrap();
    elapsed
}

fn zmq_pubsub_fanout(c: &mut Criterion) {
    let mut group = c.benchmark_group("patterns/rust_zmq/pubsub_fanout");
    for &num_subs in FANOUT_SUBSCRIBERS {
        group.throughput(Throughput::Elements((MESSAGE_COUNT * num_subs) as u64));
        group.bench_with_input(
            BenchmarkId::new("subscribers", num_subs),
            &num_subs,
            |b, &num_subs| {
                b.iter_custom(|iters| run_zmq_fanout(num_subs, iters));
            },
        );
    }
    group.finish();
}

// ─────────────────────────────────────────────────────────────────────────────
// Topic filtering (10% of published messages match the subscription)
// ─────────────────────────────────────────────────────────────────────────────
//
// Timed by matched-message delivery: the subscriber receives `iters *
// MATCHED_PER_ROUND` matching messages while the publisher oversends a 1-in-10
// match pattern. Throughput is reported per `MESSAGE_COUNT` published, so the
// number reflects the publish/filter rate the subscriber can sustain.

fn run_monocoque_topic_filtering(iters: u64) -> Duration {
    let target_matches = iters as usize * MATCHED_PER_ROUND;
    let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let pub_handle = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(pub_socket.local_addr().unwrap()).unwrap();
            pub_socket.accept_subscriber().await.unwrap();
            thread::sleep(SETTLE);
            let mut i = 0usize;
            while stop_rx.try_recv().is_err() {
                let topic = if i % 10 == 0 {
                    Bytes::from_static(b"match.topic")
                } else {
                    Bytes::from_static(b"other.topic")
                };
                pub_socket.send_frames(&[topic, payload.clone()]).await.ok();
                i += 1;
            }
        });
    });

    let server_addr = addr_rx.recv().unwrap();

    let sub_handle = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let stream = monocoque::rt::TcpStream::connect(server_addr)
                .await
                .unwrap();
            let mut sub = SubSocket::from_tcp(stream).await.unwrap();
            sub.subscribe(b"match.").await.unwrap();
            recv_n(&mut sub, MATCHED_PER_ROUND * 2).await; // untimed warmup
            let start = Instant::now();
            recv_n(&mut sub, target_matches).await;
            start.elapsed()
        })
    });

    let elapsed = sub_handle.join().unwrap();
    let _ = stop_tx.send(());
    pub_handle.join().unwrap();
    elapsed
}

fn monocoque_topic_filtering(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!(
        "patterns/monocoque-{BENCH_BACKEND}/topic_filtering"
    ));
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));
    group.bench_function("filter_10_percent", |b| {
        b.iter_custom(run_monocoque_topic_filtering);
    });
    group.finish();
}

fn run_zmq_topic_filtering(iters: u64) -> Duration {
    let target_matches = iters as usize * MATCHED_PER_ROUND;
    let payload = vec![0u8; MESSAGE_SIZE];

    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (synced_tx, synced_rx) = mpsc::channel::<()>();

    let pub_handle = thread::spawn(move || {
        let ctx = zmq::Context::new();
        let pub_socket = ctx.socket(zmq::PUB).unwrap();
        pub_socket.set_linger(0).unwrap();
        pub_socket.bind("tcp://127.0.0.1:*").unwrap();
        ep_tx
            .send(pub_socket.get_last_endpoint().unwrap().unwrap())
            .unwrap();
        // Sync phase: emit matching messages with a yield between bursts until
        // the subscriber confirms receipt (see run_zmq_fanout for the rationale).
        while synced_rx.try_recv().is_err() {
            for _ in 0..16 {
                pub_socket.send(&b"match.topic"[..], zmq::SNDMORE).unwrap();
                pub_socket.send(&payload, 0).unwrap();
            }
            thread::sleep(Duration::from_millis(1));
        }
        // Timed phase: oversend the 1-in-10 match pattern at full speed.
        let mut i = 0usize;
        while stop_rx.try_recv().is_err() {
            let topic: &[u8] = if i % 10 == 0 {
                b"match.topic"
            } else {
                b"other.topic"
            };
            pub_socket.send(topic, zmq::SNDMORE).unwrap();
            pub_socket.send(black_box(&payload), 0).unwrap();
            i += 1;
        }
    });

    let endpoint = ep_rx.recv().unwrap();
    let sub_handle = thread::spawn(move || {
        let ctx = zmq::Context::new();
        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.set_linger(0).unwrap();
        sub.connect(&endpoint).unwrap();
        sub.set_subscribe(b"match.").unwrap();
        let _ = sub.recv_bytes(0); // block until the subscription is live
        synced_tx.send(()).unwrap();
        for _ in 0..MATCHED_PER_ROUND * 2 {
            let _ = sub.recv_bytes(0);
        }
        let start = Instant::now();
        let mut count = 0;
        while count < target_matches {
            if sub.recv_bytes(0).is_ok() {
                count += 1;
            }
        }
        start.elapsed()
    });

    let elapsed = sub_handle.join().unwrap();
    let _ = stop_tx.send(());
    pub_handle.join().unwrap();
    elapsed
}

fn zmq_topic_filtering(c: &mut Criterion) {
    let mut group = c.benchmark_group("patterns/rust_zmq/topic_filtering");
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));
    group.bench_function("filter_10_percent", |b| {
        b.iter_custom(run_zmq_topic_filtering);
    });
    group.finish();
}

criterion_group!(
    name = benches;
    // Each sample re-establishes a fresh PUB/SUB connection outside the timed
    // region, so keep the sample count modest to bound total wall time.
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5))
        .sample_size(10);
    targets =
        monocoque_pubsub_fanout,
        zmq_pubsub_fanout,
        monocoque_topic_filtering,
        zmq_topic_filtering
);
criterion_main!(benches);
