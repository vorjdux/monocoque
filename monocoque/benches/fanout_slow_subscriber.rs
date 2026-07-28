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
//!
//! Multi-peer PUB fan-out over the direct-stream sockets is still maturing (see
//! the note in `benches/patterns.rs`), so counts are kept modest. This target is
//! required to COMPILE; running it to completion is best-effort.

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use monocoque::zmq::{PubSocket, SocketOptions, SubSocket};
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
/// burst. Uses `thread::sleep`, not `monocoque::rt::sleep`.
const SETTLE: Duration = Duration::from_millis(50);

fn sub_options() -> SocketOptions {
    SocketOptions::default().with_buffer_sizes(16384, 16384)
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

/// Time delivery of `iters * MESSAGE_COUNT` messages to each FAST subscriber
/// while one slow subscriber lags and the publisher oversends. Returns the
/// slowest fast subscriber's elapsed time.
fn run_fanout_slow(iters: u64) -> Duration {
    let target = iters as usize * MESSAGE_COUNT;
    let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    // Publisher: accept every subscriber, then oversend until told to stop.
    let pub_handle = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(pub_socket.local_addr().unwrap()).unwrap();
            for _ in 0..TOTAL_SUBS {
                pub_socket.accept_subscriber().await.unwrap();
            }
            thread::sleep(SETTLE);
            while stop_rx.try_recv().is_err() {
                pub_socket
                    .send_frames(std::slice::from_ref(&payload))
                    .await
                    .ok();
            }
        });
    });

    let server_addr = addr_rx.recv().unwrap();

    // One slow subscriber: reads on a fixed slow cadence, never keeping up.
    let slow_handle = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let stream = monocoque::rt::TcpStream::connect(server_addr)
                .await
                .unwrap();
            let mut sub = SubSocket::from_tcp_with_options(stream, sub_options())
                .await
                .unwrap();
            sub.subscribe(b"").await.unwrap();
            // Read slowly for the lifetime of the measurement. The fast
            // subscribers bound the iteration, so a rough cap keeps this from
            // outliving them if delivery stalls.
            for _ in 0..(WARMUP_MSGS + MESSAGE_COUNT * 64) {
                thread::sleep(SLOW_INTERVAL);
                match sub.recv().await {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        });
    });

    // Fast subscribers: drain at full speed and time their window.
    let mut fast_handles = Vec::with_capacity(FAST_SUBS);
    for _ in 0..FAST_SUBS {
        fast_handles.push(thread::spawn(move || {
            let rt = monocoque::rt::LocalRuntime::new().unwrap();
            rt.block_on(async move {
                let stream = monocoque::rt::TcpStream::connect(server_addr)
                    .await
                    .unwrap();
                let mut sub = SubSocket::from_tcp_with_options(stream, sub_options())
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

    // Slowest fast subscriber bounds delivered fan-out throughput.
    let elapsed = fast_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .max()
        .unwrap_or_default();

    let _ = stop_tx.send(());
    pub_handle.join().unwrap();
    let _ = slow_handle.join();
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
