//! C7 gate: idle sockets cost no wakeups.
//!
//! An established connection with no traffic and no heartbeat configured must
//! leave every thread of the process blocked in the kernel. A polling loop, a
//! per-operation timer, or a background task that spins shows up as context
//! switches, so the gate counts them: it sums `voluntary_ctxt_switches` over
//! every thread in `/proc/self/task` across a fixed idle window, with a
//! `recv()` parked on every receive-capable socket. A thread that wakes to poll
//! and blocks again switches voluntarily; nonvoluntary switches are preemption
//! by other load on the machine and say nothing about the socket, so they are
//! left out.
//!
//! The window is armed with one runtime timer, so the measuring thread itself
//! blocks and wakes once; the budget allows a handful of switches for that and
//! for unrelated kernel scheduling, and nothing like the hundreds a 1 ms poll
//! would produce. Linux only: elsewhere the counters do not exist and the gate
//! is skipped.
//!
//! This file also keeps the manual per-connection memory measurement
//! (`per_connection_idle_footprint`), which reports RSS and asserts nothing.
//!
//! All gate cases run inside one `#[test]` so no other test thread of this
//! binary is scheduled during a window.

// Vectors of sockets and parked receives exist only to keep them alive.
#![allow(clippy::collection_is_never_read)]

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{
    DealerSocket, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket,
    RouterSocket, SocketOptions, SubSocket, XPubSocket,
};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Length of each measured idle window.
const IDLE_WINDOW: Duration = Duration::from_millis(500);
/// Voluntary context switches allowed across the whole process in one window.
/// The window timer accounts for one or two; a 1 ms poll would cost about 500.
const SWITCH_BUDGET: u64 = 10;

/// Voluntary context switches of every thread in this process, or `None`
/// where `/proc/self/task` is unavailable.
fn process_context_switches() -> Option<u64> {
    let mut total = 0u64;
    for task in std::fs::read_dir("/proc/self/task").ok()? {
        let status = std::fs::read_to_string(task.ok()?.path().join("status")).ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("voluntary_ctxt_switches:") {
                total += rest.trim().parse::<u64>().ok()?;
            }
        }
    }
    Some(total)
}

/// Count context switches while the runtime sits idle for [`IDLE_WINDOW`].
async fn idle_switches() -> Option<u64> {
    // Let the parked receives and any just-spawned tasks settle first.
    monocoque::rt::sleep(Duration::from_millis(50)).await;
    let before = process_context_switches()?;
    monocoque::rt::sleep(IDLE_WINDOW).await;
    let after = process_context_switches()?;
    Some(after - before)
}

/// Park a receive on `socket`, keeping the task's handle in `$parked`. smol's
/// executor is thread-local and outlives a `LocalRuntime`, so a detached task
/// from one case would keep its socket (and its reconnect loop) alive into the
/// next case's window; dropping the handle at the end of the case cancels it.
macro_rules! park_recv {
    ($parked:ident, $socket:expr) => {{
        let mut socket = $socket;
        $parked.push(monocoque::rt::spawn(async move {
            let _ = socket.recv().await;
            drop(socket);
        }));
    }};
}

/// Wait until `socket` has `n` peers.
macro_rules! connected {
    ($socket:expr, $n:expr) => {{
        while $socket.peer_count() < $n {
            monocoque::rt::sleep(Duration::from_millis(1)).await;
        }
    }};
}

/// Run one case on its own runtime, so a detached receive from one case can
/// never run during another case's window. The runtime is torn down on return.
fn in_fresh_runtime(case: impl std::future::Future<Output = Option<u64>>) -> Option<u64> {
    LocalRuntime::new().unwrap().block_on(case)
}

fn check(case: &str, switches: Option<u64>, failures: &mut Vec<String>) {
    match switches {
        Some(n) if n > SWITCH_BUDGET => failures.push(format!(
            "{case}: {n} context switches in {IDLE_WINDOW:?} idle (budget {SWITCH_BUDGET})"
        )),
        Some(n) => println!("[idle wakeups] {case}: {n} switches in {IDLE_WINDOW:?}"),
        None => println!("[idle wakeups] {case}: counters unavailable, skipped"),
    }
}

#[test]
fn idle_connections_do_not_wake_the_process() {
    if process_context_switches().is_none() {
        println!("[idle wakeups] /proc/self/task unavailable; gate skipped on this platform");
        return;
    }
    let mut failures = Vec::new();

    let n = in_fresh_runtime(async {
        let mut parked = Vec::new();
        let mut router = RouterSocket::new();
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer = DealerSocket::new();
        dealer.connect(&ep).await.unwrap();
        connected!(router, 1);
        connected!(dealer, 1);
        park_recv!(parked, router);
        park_recv!(parked, dealer);
        idle_switches().await
    });
    check("DEALER <-> ROUTER", n, &mut failures);

    let n = in_fresh_runtime(async {
        let mut parked = Vec::new();
        let mut rep = RepSocket::new();
        let ep = rep.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut req = ReqSocket::new();
        req.connect(&ep).await.unwrap();
        connected!(rep, 1);
        park_recv!(parked, rep);
        let n = idle_switches().await;
        drop(req);
        n
    });
    check("REQ <-> REP", n, &mut failures);

    let n = in_fresh_runtime(async {
        let mut parked = Vec::new();
        let mut pull = PullSocket::new();
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        connected!(pull, 1);
        park_recv!(parked, pull);
        let n = idle_switches().await;
        drop(push);
        n
    });
    check("PUSH -> PULL", n, &mut failures);

    let n = in_fresh_runtime(async {
        let mut parked = Vec::new();
        let mut a = PairSocket::new();
        let ep = a.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut b = PairSocket::new();
        b.connect(&ep).await.unwrap();
        connected!(a, 1);
        connected!(b, 1);
        park_recv!(parked, a);
        park_recv!(parked, b);
        idle_switches().await
    });
    check("PAIR <-> PAIR", n, &mut failures);

    let n = in_fresh_runtime(async {
        let mut parked = Vec::new();
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::new();
        sub.subscribe(b"").await.unwrap();
        sub.connect(&ep).await.unwrap();
        connected!(publisher, 1);
        park_recv!(parked, sub);
        let n = idle_switches().await;
        drop(publisher);
        n
    });
    check("PUB -> SUB", n, &mut failures);

    let n = in_fresh_runtime(async {
        let mut parked = Vec::new();
        let mut xpub = XPubSocket::new();
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::new();
        sub.connect(&ep).await.unwrap();
        connected!(xpub, 1);
        // An XPUB waiting for a subscription event on an idle subscriber.
        park_recv!(parked, xpub);
        let n = idle_switches().await;
        drop(sub);
        n
    });
    check("XPUB waiting for a subscription", n, &mut failures);

    assert!(
        failures.is_empty(),
        "idle sockets woke the process:\n  {}",
        failures.join("\n  ")
    );
}

/// Read this process's resident set size in bytes from `/proc/self/statm`.
///
/// Returns `None` on non-Linux or if the file cannot be read/parsed, so the
/// harness degrades to "measurement unavailable" rather than failing.
fn read_rss_bytes() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    // Fields are in pages: size, resident, shared, text, lib, data, dt.
    let resident_pages: usize = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page_size = 4096usize; // Linux default; good enough for a rough per-conn figure.
    Some(resident_pages * page_size)
}

/// Manual per-connection idle memory measurement. Reports RSS, asserts nothing:
/// RSS is allocator-, kernel-, and backend-dependent, so it is not a gate.
///
/// ```sh
/// MONOCOQUE_IDLE_CONNS=10000 cargo test --features zmq --test idle_wakeups \
///     -- --ignored --nocapture
/// ```
#[test]
#[ignore = "manual idle-footprint measurement; run with --ignored --nocapture, not a gate"]
fn per_connection_idle_footprint() {
    let n: usize = std::env::var("MONOCOQUE_IDLE_CONNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let rss_before = read_rss_bytes();
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    // One ROUTER accepting N DEALERs, all idle.
    let server = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut router = RouterSocket::new();
            ep_tx
                .send(router.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            while router.peer_count() < n {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
            ready_tx.send(()).unwrap();
            while release_rx.try_recv().is_err() {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
        });
    });
    let ep = ep_rx.recv().unwrap();
    let (cli_release_tx, cli_release_rx) = mpsc::channel::<()>();
    let client = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut held = Vec::with_capacity(n);
            for _ in 0..n {
                let mut dealer = DealerSocket::new();
                dealer.connect(&ep).await.unwrap();
                held.push(dealer);
            }
            while cli_release_rx.try_recv().is_err() {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
        });
    });
    ready_rx.recv().unwrap();
    match (rss_before, read_rss_bytes()) {
        (Some(before), Some(after)) => {
            let delta = after.saturating_sub(before);
            println!(
                "[C7 idle footprint] connections={n} delta={delta} bytes \
                 per_connection(both_ends)={:.0} bytes",
                delta as f64 / n as f64
            );
        }
        _ => println!("[C7 idle footprint] RSS unavailable; held {n} idle connections"),
    }
    let _ = SocketOptions::default();
    release_tx.send(()).unwrap();
    cli_release_tx.send(()).unwrap();
    server.join().unwrap();
    client.join().unwrap();
}
