//! Allocation gate: no socket type allocates per message, in either direction.
//!
//! A counting global allocator tallies every allocation and reallocation in the
//! process while a measured window of messages flows. Sender and receiver run in
//! the same process, so the count covers both directions. Each case warms the
//! connection to steady state first, so buffers that grow once are not counted.
//!
//! What may allocate is the amortized growth of a reused buffer: a read buffer
//! that still has messages split off it is replaced once it fills, and the
//! region fan-out encodings are carved from is replaced every 64 KiB. Neither
//! scales with the message count, so the ceiling is one allocation per 50
//! messages: far above that residue, far below the one-per-message a
//! regression would show.
//!
//! The last case is the inline-message guarantee: a queue of small messages
//! waiting to be received holds no read buffer, because each one was copied
//! into its `Msg` and the read buffer was reused.

// The round closures need `mut` only where the backend's futures capture
// mutable state.
#![allow(unused_mut)]

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{
    DealerSocket, Msg, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket,
    RouterSocket, SocketOptions, StreamSocket, SubSocket,
};
use monocoque_zmtp::security::curve::CurveSecretKey;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicUsize = AtomicUsize::new(0);
/// Cases share the global counter, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Counting;

// SAFETY: delegates every operation to the system allocator unchanged; the
// atomics only observe, they never touch the returned pointers.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) != 0 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) != 0 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const WARM: usize = 2_000;
const MEAS: usize = 50_000;
/// Ceiling for our own code: one allocation per ten messages. Above-inline
/// frames are split off the read buffer, which is replaced once it fills while
/// still referenced (measured: one per 12 messages of 1 KiB); everything else
/// measures at or near zero.
const CEILING: usize = MEAS / 10;

/// compio's driver heap-allocates every submitted operation (its `ThinCell`
/// op record), about 1.5 allocations per read or write. That is driver cost
/// per I/O, not ours, and the io_uring-native transport (multishot receive,
/// pooled write ops) removes it; until then compio is bounded per operation
/// and `runtime_is_allocation_free` below waits, ignored, for that work.
/// smol's async-io reactor allocates once per readiness wait (measured: one
/// per read or write that has to wait), which is likewise the runtime's cost.
#[cfg(feature = "runtime-compio")]
const PER_OP_DRIVER_ALLOCS: usize = 2;
#[cfg(feature = "runtime-smol")]
const PER_OP_DRIVER_ALLOCS: usize = 1;
#[cfg(feature = "runtime-tokio")]
const PER_OP_DRIVER_ALLOCS: usize = 0;

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(10))
}

fn start() {
    ALLOCS.store(0, Ordering::Relaxed);
    COUNTING.store(1, Ordering::Relaxed);
}

/// End the window. `ops` is the reads plus writes each measured unit costs,
/// which is what the compio driver allocates for.
fn stop(case: &str, ops: usize) -> usize {
    stop_owning(case, ops, 0)
}

/// [`stop`] for a case whose messages each own `owned` allocations of their
/// own, which are the message's memory rather than per-message overhead.
fn stop_owning(case: &str, ops: usize, owned: usize) -> usize {
    COUNTING.store(0, Ordering::Relaxed);
    let n = ALLOCS.load(Ordering::Relaxed);
    let ceiling = CEILING + (PER_OP_DRIVER_ALLOCS * ops + owned) * MEAS;
    println!("[hotpath_alloc] {case}: {n} allocations over {MEAS} (ceiling {ceiling})");
    assert!(
        n <= ceiling,
        "{case}: {n} allocations over {MEAS} (ceiling {ceiling}); \
         something allocates per message"
    );
    n
}

/// Run `body` on a fresh runtime with the counter to itself.
fn case(body: impl std::future::Future<Output = ()>) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    LocalRuntime::new().unwrap().block_on(body);
}

#[test]
fn push_pull_single_frame() {
    case(async {
        let mut pull = PullSocket::with_options(opts().with_recv_hwm(0));
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![7u8; 64]);
        for _ in 0..WARM {
            push.send(payload.clone()).await.unwrap();
            pull.recv().await.unwrap();
        }
        start();
        for _ in 0..MEAS {
            push.send(payload.clone()).await.unwrap();
            pull.recv().await.unwrap();
        }
        stop("PUSH -> PULL 64 B", 2);
    });
}

#[test]
fn req_rep_round_trip() {
    case(async {
        let mut rep = RepSocket::with_options(opts());
        let ep = rep.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut req = ReqSocket::with_options(opts());
        req.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![7u8; 64]);
        let reply = Msg::from(vec![8u8; 64]);
        let mut round = async |req: &mut ReqSocket, rep: &mut RepSocket| {
            req.send(payload.clone()).await.unwrap();
            rep.recv().await.unwrap();
            rep.send(reply.clone()).await.unwrap();
            req.recv().await.unwrap();
        };
        for _ in 0..WARM {
            round(&mut req, &mut rep).await;
        }
        start();
        for _ in 0..MEAS {
            round(&mut req, &mut rep).await;
        }
        stop("REQ <-> REP 64 B", 4);
    });
}

#[test]
fn dealer_router_multi_frame() {
    case(async {
        let mut router = RouterSocket::with_options(opts());
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer =
            DealerSocket::with_options(opts().with_routing_id(Bytes::from_static(b"d1")));
        dealer.connect(&ep).await.unwrap();
        let request = Msg::from([Bytes::from_static(b"head"), Bytes::from_static(&[1u8; 200])]);
        let mut round = async |dealer: &mut DealerSocket, router: &mut RouterSocket| {
            dealer.send(request.clone()).await.unwrap();
            let m = router.recv_owned().await.unwrap();
            router.send(m).await.unwrap();
            dealer.recv().await.unwrap();
        };
        for _ in 0..WARM {
            round(&mut dealer, &mut router).await;
        }
        start();
        for _ in 0..MEAS {
            round(&mut dealer, &mut router).await;
        }
        stop("DEALER <-> ROUTER 2 frames", 4);
    });
}

#[test]
fn pair_large_frames() {
    case(async {
        let mut a = PairSocket::with_options(opts());
        let ep = a.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut b = PairSocket::with_options(opts());
        b.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![3u8; 1024]);
        for _ in 0..WARM {
            b.send(payload.clone()).await.unwrap();
            a.recv().await.unwrap();
        }
        start();
        for _ in 0..MEAS {
            b.send(payload.clone()).await.unwrap();
            a.recv().await.unwrap();
        }
        stop("PAIR 1 KiB", 2);
    });
}

/// Frames above the vectored threshold go to the kernel as their own iovec.
#[test]
fn pair_vectored_frames() {
    case(async {
        let mut a = PairSocket::with_options(opts());
        let ep = a.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut b = PairSocket::with_options(opts());
        b.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![3u8; 64 * 1024]);
        for _ in 0..WARM {
            b.send(payload.clone()).await.unwrap();
            a.recv().await.unwrap();
        }
        start();
        for _ in 0..MEAS {
            b.send(payload.clone()).await.unwrap();
            a.recv().await.unwrap();
        }
        // A frame too long for the read buffer is read into one allocated
        // for it, which the message then owns: one per message until receives
        // come from a registered buffer pool. Regrowing and sharing the read
        // buffer instead cost three. A 64 KiB frame takes several reads;
        // budget four per message for the driver.
        stop_owning("PAIR 64 KiB vectored", 5, 1);
    });
}

/// Coalesced sends in flushed bursts, with a send timeout set so the timer
/// path is measured.
#[test]
fn push_pull_coalesced_with_timeouts() {
    const BURST: usize = 64;
    case(async {
        let mut pull = PullSocket::with_options(opts().with_recv_hwm(0));
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::with_options(
            opts()
                .with_write_coalescing(true)
                .with_send_timeout(Duration::from_secs(10)),
        );
        push.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![7u8; 64]);
        let mut burst = async |push: &mut PushSocket, pull: &mut PullSocket| {
            for _ in 0..BURST {
                push.send(payload.clone()).await.unwrap();
            }
            push.flush().await.unwrap();
            for _ in 0..BURST {
                pull.recv().await.unwrap();
            }
        };
        for _ in 0..WARM / BURST {
            burst(&mut push, &mut pull).await;
        }
        start();
        for _ in 0..MEAS / BURST {
            burst(&mut push, &mut pull).await;
        }
        stop("PUSH -> PULL coalesced, timeouts", 2);
    });
}

#[test]
fn curve_push_pull() {
    let key = |seed: u8| {
        let secret = [seed; 32];
        (
            *CurveSecretKey::from_bytes(secret).public_key().as_bytes(),
            secret,
        )
    };
    let (server_public, server_secret) = key(1);
    let (client_public, client_secret) = key(2);
    case(async {
        let mut pull = PullSocket::with_options(
            opts()
                .with_recv_hwm(0)
                .with_curve_server(true)
                .with_curve_keypair(server_public, server_secret),
        );
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::with_options(
            SocketOptions::default()
                .with_curve_keypair(client_public, client_secret)
                .with_curve_serverkey(server_public),
        );
        push.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![7u8; 64]);
        for _ in 0..WARM {
            push.send(payload.clone()).await.unwrap();
            pull.recv().await.unwrap();
        }
        start();
        for _ in 0..MEAS {
            push.send(payload.clone()).await.unwrap();
            pull.recv().await.unwrap();
        }
        stop("CURVE PUSH -> PULL 64 B", 2);
    });
}

/// STREAM to STREAM: raw bytes, routed by identity on both sides.
#[test]
fn stream_round_trip() {
    case(async {
        let mut server = StreamSocket::with_options(opts());
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client = StreamSocket::with_options(opts());
        client.connect(&ep).await.unwrap();
        let to_server = client.recv_owned().await.unwrap().first().unwrap().to_vec();
        let to_client = server.recv_owned().await.unwrap().first().unwrap().to_vec();
        let ping = Msg::from([Bytes::from(to_server), Bytes::from_static(&[1u8; 64])]);
        let pong = Msg::from([Bytes::from(to_client), Bytes::from_static(&[2u8; 64])]);
        // Each side reads exactly what one send wrote, since the other waits.
        let mut round = async |client: &mut StreamSocket, server: &mut StreamSocket| {
            client.send(ping.clone()).await.unwrap();
            server.recv().await.unwrap();
            server.send(pong.clone()).await.unwrap();
            client.recv().await.unwrap();
        };
        for _ in 0..WARM {
            round(&mut client, &mut server).await;
        }
        start();
        for _ in 0..MEAS {
            round(&mut client, &mut server).await;
        }
        stop("STREAM <-> STREAM 64 B", 4);
    });
}

#[test]
fn pub_fanout_to_two_subscribers() {
    case(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut subs = [
            SubSocket::with_options(opts().with_recv_hwm(0)),
            SubSocket::with_options(opts().with_recv_hwm(0)),
        ];
        for s in &mut subs {
            s.subscribe(b"t").await.unwrap();
            s.connect(&ep).await.unwrap();
        }
        let msg = Msg::from([Bytes::from_static(b"topic"), Bytes::from_static(&[5u8; 64])]);
        while publisher.send(msg.clone()).await.unwrap() < 2 {
            monocoque::rt::sleep(Duration::from_millis(1)).await;
        }
        for s in &mut subs {
            s.recv().await.unwrap();
        }
        let mut round = async |publisher: &mut PubSocket, subs: &mut [SubSocket; 2]| {
            assert_eq!(publisher.send(msg.clone()).await.unwrap(), 2);
            for s in subs.iter_mut() {
                s.recv().await.unwrap();
            }
        };
        for _ in 0..WARM {
            round(&mut publisher, &mut subs).await;
        }
        start();
        for _ in 0..MEAS {
            round(&mut publisher, &mut subs).await;
        }
        stop("PUB -> 2 x SUB", 4);
    });
}

#[test]
fn queued_inline_messages_retain_no_read_buffer() {
    case(async {
        let mut pull = PullSocket::with_options(opts().with_recv_hwm(0));
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        push.send(Msg::from("warm")).await.unwrap();
        pull.recv().await.unwrap();
        // Let the queue reach its final size once so only buffer retention,
        // not queue growth, is measured below.
        let payload = Msg::from(vec![1u8; 48]);
        for _ in 0..MEAS {
            push.send(payload.clone()).await.unwrap();
        }
        push.flush().await.unwrap();
        for _ in 0..MEAS {
            pull.recv().await.unwrap();
        }
        // Now queue MEAS inline messages without receiving them. Had each held
        // a slice of its read buffer, every filled buffer would be replaced:
        // about one allocation per read buffer's worth of messages.
        start();
        for _ in 0..MEAS {
            push.send(payload.clone()).await.unwrap();
        }
        push.flush().await.unwrap();
        while !pull.has_message() {
            monocoque::rt::sleep(Duration::from_millis(1)).await;
        }
        monocoque::rt::sleep(Duration::from_millis(50)).await;
        // One write and one read per message, as in PUSH -> PULL.
        let n = stop("queue of inline messages", 2);
        // Only tokio's I/O path allocates nothing of its own, so only there
        // is the count a clean measure of buffer retention.
        if PER_OP_DRIVER_ALLOCS == 0 {
            assert!(
                n < 20,
                "queued inline messages held read buffers: {n} allocations"
            );
        }
        for _ in 0..MEAS {
            pull.recv().await.unwrap();
        }
    });
}

/// The strict form of the gate on compio: no driver allowance at all.
#[test]
#[cfg(not(feature = "runtime-tokio"))]
#[ignore = "compio allocates an op record per read and write and smol's reactor one per readiness wait; the io_uring-native transport (multishot receive, pooled write ops) removes the compio cost"]
fn runtime_is_allocation_free() {
    case(async {
        let mut pull = PullSocket::with_options(opts().with_recv_hwm(0));
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![7u8; 64]);
        for _ in 0..WARM {
            push.send(payload.clone()).await.unwrap();
            pull.recv().await.unwrap();
        }
        start();
        for _ in 0..MEAS {
            push.send(payload.clone()).await.unwrap();
            pull.recv().await.unwrap();
        }
        COUNTING.store(0, Ordering::Relaxed);
        let n = ALLOCS.load(Ordering::Relaxed);
        assert!(n <= CEILING, "compio: {n} allocations over {MEAS} messages");
    });
}
