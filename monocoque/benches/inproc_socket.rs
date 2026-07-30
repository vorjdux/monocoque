//! Inproc SOCKET-path benchmark.
//!
//! This exercises the full DEALER socket send/recv path over an `inproc://`
//! endpoint, not a raw flume channel. Both peers are real `DealerSocket`s whose
//! bytes travel through `InprocStream` (the in-process AsyncRead/AsyncWrite
//! adapter), so every message is ZMTP-framed by the sender and decoded by the
//! receiver exactly as it would be over TCP - only the transport is swapped for
//! the zero-syscall in-process channel.
//!
//! ## Why DEALER and not PAIR
//!
//! DEALER exposes a bidi inproc pair that round-trips correctly:
//! `bind_inproc_bidi` on the server registers a reply channel that
//! `connect_inproc` on the client reads its replies from. (The PAIR inproc
//! constructors pair a non-bidi bind with a bidi connect, so they do not form a
//! working round-trip pair; DEALER is the reliable full-socket inproc path.)
//!
//! ## Methodology
//!
//! - Server and client each run on their own OS thread with their own runtime.
//! - Endpoint setup (bind, connect) happens outside the timed window; the client
//!   waits on a channel until the server has bound before it connects.
//! - `roundtrip` times a ping-pong: the client sends one 64B frame and waits for
//!   the server to echo it back. This is naturally rate-limited (no unbounded
//!   buffering) and measures the socket send + recv cost in both directions.
//! - Each endpoint name is unique per iteration batch so a fresh bind never
//!   collides with a not-yet-unbound previous one.

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use monocoque_core::options::SocketOptions;
// `InprocStream` (the concrete stream type of the inproc DEALER) is a private
// module in monocoque-zmtp, so it cannot be named here. The `bind_inproc_bidi`
// and `connect_inproc` methods exist only on the `DealerSocket<InprocStream>`
// impl, so type inference resolves the stream type without naming it.
use monocoque_zmtp::DealerSocket;
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
    // Default 32 KB read batch. Coalescing does not apply to a ping-pong (each
    // send must reach the peer for the reply), so the 0.4 win here is the
    // allocation-free receive path used below.
    SocketOptions::default()
}

/// Time `iters` round-trips of a single 64B frame over the inproc DEALER pair.
fn run_roundtrip(iters: u64) -> Duration {
    let seq = ENDPOINT_SEQ.fetch_add(1, Ordering::Relaxed);
    let endpoint = format!("inproc://bench-inproc-socket-{seq}");
    let payload = Bytes::from(vec![0u8; PAYLOAD]);

    let (ready_tx, ready_rx) = mpsc::channel::<()>();

    let server_endpoint = endpoint.clone();
    let server = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut server = DealerSocket::bind_inproc_bidi(&server_endpoint, options()).unwrap();
            ready_tx.send(()).unwrap();
            // Echo every frame back allocation-free until the endpoint is
            // unbound (recv -> EOF). Dropping the client alone does NOT end this
            // loop: the inproc registry keeps a sender clone alive, so the server
            // stays readable until unbind_inproc removes the endpoint.
            let mut buf: Vec<Bytes> = Vec::with_capacity(4);
            loop {
                match server.recv_into(&mut buf).await {
                    Ok(true) => {
                        server.send(buf.clone()).await.unwrap();
                    }
                    Ok(false) | Err(_) => break,
                }
            }
        });
    });

    ready_rx.recv().unwrap();

    let client_endpoint = endpoint.clone();
    let elapsed = {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut client = DealerSocket::connect_inproc(&client_endpoint, options()).unwrap();

            // Warm the pipe so the first send/recv buffer growth is untimed.
            let mut buf: Vec<Bytes> = Vec::with_capacity(4);
            client.send(vec![payload.clone()]).await.unwrap();
            let _ = client.recv_into(&mut buf).await.unwrap();

            let start = Instant::now();
            for _ in 0..iters {
                client.send(vec![payload.clone()]).await.unwrap();
                let _ = client.recv_into(&mut buf).await.unwrap();
            }
            let elapsed = start.elapsed();
            // Drop the client's sender; the endpoint is torn down by the unbind
            // below (before the server join) so the server loop can exit.
            drop(client);
            elapsed
        })
    };

    // Unbind BEFORE joining. The registry holds a sender clone that keeps the
    // server's recv alive, so the server's echo loop only sees EOF once the
    // endpoint is removed. Joining first (the old order) deadlocks: the server
    // never exits, so join blocks forever and the unbind is never reached.
    let _ = monocoque_core::inproc::unbind_inproc(&endpoint);
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
