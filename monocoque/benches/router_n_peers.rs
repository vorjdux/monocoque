//! ROUTER throughput as the number of connected DEALER peers grows.
//!
//! One ROUTER endpoint accepts N DEALER connections and drains a fixed number of
//! messages from every peer. The benchmark parameter is N (1, 8, 64), so the
//! reported per-element cost shows how ROUTER receive throughput scales as more
//! peers are multiplexed.
//!
//! ## Architecture note
//!
//! monocoque's `RouterSocket` is one-connection-per-socket: a single accepted
//! stream becomes one `RouterSocket`, and each `recv()` yields
//! `[peer_identity, payload]`. To model "one ROUTER with N peers" we bind one
//! listener, accept N streams, and hold N `RouterSocket`s that are drained
//! concurrently on one runtime via `monocoque::rt::spawn`. This measures the
//! aggregate receive throughput one ROUTER endpoint sustains across N peers.
//!
//! ## Methodology
//!
//! - Each DEALER peer runs on its own OS thread with its own runtime, connects,
//!   and sends `MESSAGES_PER_PEER` frames of 64B. Each connection gets an
//!   auto-assigned identity from the ROUTER, so N connections are N distinct
//!   peers.
//! - Connection setup and the ZMTP handshake (accept + `from_tcp`) happen before
//!   the timer starts. The timed window is the concurrent drain of all N * K
//!   messages on the ROUTER side.
//! - The DEALER sends overlap with the drain (messages buffer in the kernel until
//!   drained), which is the realistic under-load receive path.

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use monocoque::rt::TcpListener;
use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
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

const PEER_COUNTS: &[usize] = &[1, 8, 64];
const MESSAGES_PER_PEER: usize = 2_000;
const PAYLOAD: usize = 64;

fn options() -> SocketOptions {
    SocketOptions::default().with_buffer_sizes(16384, 16384)
}

/// Drive N DEALER peers into one ROUTER endpoint and time draining all messages.
fn run_router(num_peers: usize, iters: u64) -> Duration {
    let mut total = Duration::ZERO;

    for _ in 0..iters {
        let (port_tx, port_rx) = mpsc::channel::<u16>();
        let payload = Bytes::from(vec![0u8; PAYLOAD]);

        // ROUTER side: bind, accept N streams, drain all messages concurrently.
        let router_thread = thread::spawn(move || {
            let rt = monocoque::rt::LocalRuntime::new().unwrap();
            rt.block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                port_tx.send(listener.local_addr().unwrap().port()).unwrap();

                // Accept and handshake every peer before timing.
                let mut routers = Vec::with_capacity(num_peers);
                for _ in 0..num_peers {
                    let (stream, _) = listener.accept().await.unwrap();
                    let router = RouterSocket::from_tcp_with_options(stream, options())
                        .await
                        .unwrap();
                    routers.push(router);
                }

                // Timed window: drain MESSAGES_PER_PEER from each peer concurrently.
                let start = Instant::now();
                let mut handles = Vec::with_capacity(num_peers);
                for mut router in routers {
                    handles.push(monocoque::rt::spawn(async move {
                        let mut got = 0usize;
                        while got < MESSAGES_PER_PEER {
                            match router.recv().await {
                                Ok(Some(_)) => got += 1,
                                Ok(None) | Err(_) => break,
                            }
                        }
                        got
                    }));
                }
                for handle in handles {
                    let _ = handle.await;
                }
                start.elapsed()
            })
        });

        let port = port_rx.recv().unwrap();

        // DEALER peers: each connects and sends its share.
        let mut peers = Vec::with_capacity(num_peers);
        for _ in 0..num_peers {
            let peer_payload = payload.clone();
            peers.push(thread::spawn(move || {
                let rt = monocoque::rt::LocalRuntime::new().unwrap();
                rt.block_on(async move {
                    let mut dealer =
                        DealerSocket::connect_with_options(&format!("127.0.0.1:{port}"), options())
                            .await
                            .unwrap();
                    for _ in 0..MESSAGES_PER_PEER {
                        dealer.send(vec![peer_payload.clone()]).await.unwrap();
                    }
                    dealer.flush().await.unwrap();
                });
            }));
        }

        for peer in peers {
            peer.join().unwrap();
        }
        total += router_thread.join().unwrap();
    }

    total
}

fn router_n_peers(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("router_n_peers/monocoque-{BENCH_BACKEND}"));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    for &num_peers in PEER_COUNTS {
        group.throughput(Throughput::Elements((num_peers * MESSAGES_PER_PEER) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(num_peers),
            &num_peers,
            |b, &num_peers| {
                b.iter_custom(|iters| run_router(num_peers, iters));
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .warm_up_time(Duration::from_secs(2))
        .sample_size(10);
    targets = router_n_peers
);
criterion_main!(benches);
