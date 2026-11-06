//! Inproc SOCKET-path benchmark.
//!
//! This exercises the full DEALER socket send/recv path over an `inproc://`
//! endpoint, not a raw channel. Both peers are real `DealerSocket`s; the two
//! sockets live on different threads, so messages cross between them as `Msg`
//! values with no codec and no syscalls. This measures the socket send + recv
//! cost with the transport reduced to the in-process channel.
//!
//! ## Methodology
//!
//! - Server and client each run on their own OS thread with their own runtime.
//! - Endpoint setup (bind, connect) and one warmup round-trip happen outside
//!   the timed window; the client waits on a channel until the server has bound
//!   before it connects.
//! - `roundtrip` times a ping-pong: the client sends one 64B frame and waits for
//!   the server to echo it back. This is naturally rate-limited (no unbounded
//!   buffering) and measures the socket send + recv cost in both directions.
//! - Each endpoint name is unique per iteration batch so a fresh bind never
//!   collides with a not-yet-released previous one.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use monocoque::zmq::{DealerSocket, Msg, SocketOptions};
use std::sync::atomic::{AtomicU64, Ordering};
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

const PAYLOAD: usize = 64;
static ENDPOINT_SEQ: AtomicU64 = AtomicU64::new(0);

fn options() -> SocketOptions {
    // Coalescing does not apply to a ping-pong (each send must reach the peer
    // for the reply), so this uses the defaults.
    SocketOptions::default()
}

/// Time `iters` round-trips of a single 64B frame over the inproc DEALER pair.
fn run_roundtrip(iters: u64) -> Duration {
    let seq = ENDPOINT_SEQ.fetch_add(1, Ordering::Relaxed);
    let endpoint = format!("inproc://bench-inproc-socket-{seq}");
    let payload = Msg::from(vec![0u8; PAYLOAD]);
    // The warmup round-trip plus every timed one.
    let total = 1 + iters;

    let (ready_tx, ready_rx) = mpsc::channel::<()>();

    let server_endpoint = endpoint.clone();
    let server = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut server = DealerSocket::with_options(options());
            server.bind(&server_endpoint).await.unwrap();
            ready_tx.send(()).unwrap();
            // Echo every message back; it is moved, never copied.
            for _ in 0..total {
                let Ok(msg) = server.recv_owned().await else {
                    break;
                };
                if server.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = server.close().await;
        });
    });

    ready_rx.recv().unwrap();

    let elapsed = {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut client = DealerSocket::with_options(options());
            client.connect(&endpoint).await.unwrap();

            // Warm the pipe so connection setup and first buffer growth are untimed.
            client.send(payload.clone()).await.unwrap();
            let _ = client.recv().await.unwrap();

            let start = Instant::now();
            for _ in 0..iters {
                client.send(payload.clone()).await.unwrap();
                let _ = client.recv().await.unwrap();
            }
            start.elapsed()
        })
    };

    server.join().unwrap();
    elapsed
}

fn inproc_socket_roundtrip(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("inproc_socket/monocoque-{BENCH_BACKEND}"));
    group.measurement_time(Duration::from_secs(5));
    group.warm_up_time(Duration::from_secs(1));
    group.sample_size(10);
    // One round-trip == one element delivered each way; count the ping.
    group.throughput(Throughput::Elements(1));
    group.bench_function("roundtrip_64B", |b| {
        b.iter_custom(run_roundtrip);
    });
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(5))
        .warm_up_time(Duration::from_secs(1))
        .sample_size(10);
    targets = inproc_socket_roundtrip
);
criterion_main!(benches);
