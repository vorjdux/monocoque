//! ROUTER throughput as the number of connected DEALER peers grows.
//!
//! One ROUTER endpoint accepts N DEALER connections and drains a fixed number of
//! messages from every peer. The benchmark parameter is N (1, 8, 64), so the
//! reported per-element cost shows how ROUTER receive throughput scales as more
//! peers are multiplexed.
//!
//! ## Architecture note
//!
//! One `RouterSocket` binds the endpoint and multiplexes all N connections;
//! each `recv()` yields `[peer_identity, payload]` from whichever peer is
//! ready, fair-queued. This measures the aggregate receive throughput one
//! ROUTER endpoint sustains across N peers.
//!
//! ## Methodology
//!
//! - Each DEALER peer runs on its own OS thread with its own runtime, connects,
//!   and sends `MESSAGES_PER_PEER` frames of 64B. Each connection gets an
//!   auto-assigned identity from the ROUTER, so N connections are N distinct
//!   peers.
//! - Connection setup and the ZMTP handshake happen before the timer starts:
//!   the ROUTER waits until all N peers are connected. The timed window is the concurrent drain of all N * K
//!   messages on the ROUTER side.
//! - The DEALER sends overlap with the drain (messages buffer in the kernel until
//!   drained), which is the realistic under-load receive path.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use monocoque::zmq::{DealerSocket, Msg, RouterSocket, SocketOptions};
use std::sync::{Arc, Barrier, mpsc};
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
const COALESCE_BATCH: usize = 256;

// Recommended throughput usage for the DEALER senders: coalesce sends into one
// buffer and let the read batch default to 32 KB instead of pinning 16 KB.
// Coalesced writes are held until a flush, so the sender flushes every
// COALESCE_BATCH messages, well under the default send HWM of 1000.
fn dealer_options() -> SocketOptions {
    SocketOptions::default().with_write_coalescing(true)
}

// ROUTER receiver: default options (32 KB read batch). It drains with the
// borrowed recv below, so it allocates nothing per message.
fn router_options() -> SocketOptions {
    SocketOptions::default()
}

/// Drive N DEALER peers into one ROUTER endpoint and time draining all messages.
fn run_router(num_peers: usize, iters: u64) -> Duration {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    // The ROUTER reaches it once it has drained everything and every DEALER
    // waits on it, so no peer disconnects while its messages are in flight.
    let done = Arc::new(Barrier::new(num_peers + 1));
    let router_done = Arc::clone(&done);
    let payload = Msg::from(vec![0u8; PAYLOAD]);

    // One set of N persistent connections for the whole measurement. Each peer
    // sends `iters * MESSAGES_PER_PEER` messages and the ROUTER drains that many,
    // timed once. Reusing the connections (instead of rebuilding them per
    // iteration) keeps the ephemeral port range from filling with TIME_WAIT and
    // failing a later bind with AddrInUse once the drain is fast.
    let total_per_peer =
        usize::try_from(iters).expect("iteration count fits usize") * MESSAGES_PER_PEER;

    // ROUTER side: bind, wait for N peers, drain all messages.
    let router_thread = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut router = RouterSocket::with_options(router_options());
            ep_tx
                .send(router.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();

            // Every peer is connected and handshaken before timing.
            while router.peer_count() < num_peers {
                monocoque::rt::sleep(Duration::from_millis(1)).await;
            }

            let total = total_per_peer * num_peers;
            let start = Instant::now();
            for _ in 0..total {
                if router.recv().await.is_err() {
                    break;
                }
            }
            let elapsed = start.elapsed();
            router_done.wait();
            elapsed
        })
    });

    let endpoint = ep_rx.recv().unwrap();

    // DEALER peers: each connects once and sends its full share.
    let mut peers = Vec::with_capacity(num_peers);
    for _ in 0..num_peers {
        let peer_payload = payload.clone();
        let endpoint = endpoint.clone();
        let done = Arc::clone(&done);
        peers.push(thread::spawn(move || {
            let rt = monocoque::rt::LocalRuntime::new().unwrap();
            rt.block_on(async move {
                let mut dealer = DealerSocket::with_options(dealer_options());
                dealer.connect(&endpoint).await.unwrap();
                for i in 1..=total_per_peer {
                    dealer.send(peer_payload.clone()).await.unwrap();
                    if i % COALESCE_BATCH == 0 {
                        dealer.flush().await.unwrap();
                    }
                }
                dealer.flush().await.unwrap();
                // Keep the connection up until the ROUTER has drained it.
                done.wait();
                let _ = dealer.close().await;
            });
        }));
    }

    let elapsed = router_thread.join().unwrap();
    for peer in peers {
        peer.join().unwrap();
    }
    elapsed
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
