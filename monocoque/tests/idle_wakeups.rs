//! C7 gate: idle sockets cost no wakeups.
//!
//! An established connection with no traffic and no heartbeat configured must
//! leave every thread of the process blocked in the kernel. A polling loop, a
//! per-operation timer, or a background task that spins shows up as context
//! switches, so the gate counts them: it sums `voluntary_ctxt_switches` and
//! `nonvoluntary_ctxt_switches` over every thread in `/proc/self/task` across a
//! fixed idle window, with a `recv()` parked on every receive-capable socket.
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

use monocoque::rt::{LocalRuntime, TcpListener, TcpStream};
use monocoque::zmq::{
    DealerSocket, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket,
    RouterSocket, SocketOptions, SubSocket,
};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Length of each measured idle window.
const IDLE_WINDOW: Duration = Duration::from_millis(500);
/// Context switches allowed across the whole process in one window. The window
/// timer accounts for one or two; a 1 ms poll would cost about 500.
const SWITCH_BUDGET: u64 = 10;

/// Total context switches of every thread in this process, or `None` where
/// `/proc/self/task` is unavailable.
fn process_context_switches() -> Option<u64> {
    let mut total = 0u64;
    for task in std::fs::read_dir("/proc/self/task").ok()? {
        let status = std::fs::read_to_string(task.ok()?.path().join("status")).ok()?;
        for line in status.lines() {
            if let Some(rest) = line
                .strip_prefix("voluntary_ctxt_switches:")
                .or_else(|| line.strip_prefix("nonvoluntary_ctxt_switches:"))
            {
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

fn opts() -> SocketOptions {
    SocketOptions::default()
}

/// Accept one TCP connection on a fresh listener while `connect` runs, and
/// return both streams.
async fn tcp_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept = monocoque::rt::spawn(async move { listener.accept().await.unwrap().0 });
    let client = TcpStream::connect(addr).await.unwrap();
    let server = monocoque::rt::join(accept).await;
    (server, client)
}

/// Park a receive on `socket` for the rest of the runtime's life.
macro_rules! park_recv {
    ($socket:expr) => {{
        let mut socket = $socket;
        monocoque::rt::spawn_detached(async move {
            let _ = socket.recv().await;
            drop(socket);
        });
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
        let (server, client) = tcp_pair().await;
        let connect = monocoque::rt::spawn(DealerSocket::from_tcp_with_options(client, opts()));
        let router = RouterSocket::from_tcp_with_options(server, opts())
            .await
            .unwrap();
        let dealer = monocoque::rt::join(connect).await.unwrap();
        park_recv!(router);
        park_recv!(dealer);
        idle_switches().await
    });
    check("DEALER <-> ROUTER", n, &mut failures);

    let n = in_fresh_runtime(async {
        let (server, client) = tcp_pair().await;
        let connect = monocoque::rt::spawn(ReqSocket::from_tcp_with_options(client, opts()));
        let rep = RepSocket::from_tcp_with_options(server, opts())
            .await
            .unwrap();
        let _req = monocoque::rt::join(connect).await.unwrap();
        park_recv!(rep);
        idle_switches().await
    });
    check("REQ <-> REP", n, &mut failures);

    let n = in_fresh_runtime(async {
        let (server, client) = tcp_pair().await;
        let connect = monocoque::rt::spawn(PushSocket::from_tcp_with_options(client, opts()));
        let pull = PullSocket::from_tcp_with_options(server, opts())
            .await
            .unwrap();
        let _push = monocoque::rt::join(connect).await.unwrap();
        park_recv!(pull);
        idle_switches().await
    });
    check("PUSH -> PULL", n, &mut failures);

    let n = in_fresh_runtime(async {
        let (server, client) = tcp_pair().await;
        let connect = monocoque::rt::spawn(PairSocket::from_tcp_with_options(client, opts()));
        let a = PairSocket::from_tcp_with_options(server, opts())
            .await
            .unwrap();
        let b = monocoque::rt::join(connect).await.unwrap();
        park_recv!(a);
        park_recv!(b);
        idle_switches().await
    });
    check("PAIR <-> PAIR", n, &mut failures);

    let n = in_fresh_runtime(async {
        let mut publisher = PubSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = publisher.local_addr().unwrap();
        let connect = monocoque::rt::spawn(async move {
            let mut sub = SubSocket::connect_with_options(&addr.to_string(), opts())
                .await
                .unwrap();
            sub.subscribe(b"").await.unwrap();
            sub
        });
        publisher.accept_subscriber().await.unwrap();
        let sub = monocoque::rt::join(connect).await;
        park_recv!(sub);
        let n = idle_switches().await;
        drop(publisher);
        n
    });
    check("PUB -> SUB", n, &mut failures);

    assert!(
        failures.is_empty(),
        "idle sockets woke the process:\n  {}",
        failures.join("\n  ")
    );
}

/// XPUB waiting for a subscription event on an idle subscriber.
///
/// Run it alone (`cargo test --test idle_wakeups idle_xpub -- --ignored`) so no
/// other test thread runs during its window.
#[test]
#[ignore = "XPUB::recv_subscription polls each subscriber under a 1 ms read timeout, so waiting for an event busy-loops; the event-driven driver removes the poll"]
fn idle_xpub_does_not_wake_the_process() {
    if process_context_switches().is_none() {
        return;
    }
    let rt = LocalRuntime::new().unwrap();
    let n = rt.block_on(async {
        let mut xpub = monocoque::zmq::XPubSocket::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = xpub.local_addr().unwrap();
        let connect = monocoque::rt::spawn(async move {
            SubSocket::connect_with_options(&addr.to_string(), opts())
                .await
                .unwrap()
        });
        xpub.accept().await.unwrap();
        let _sub = monocoque::rt::join(connect).await;
        monocoque::rt::spawn_detached(async move {
            while matches!(xpub.recv_subscription().await, Ok(None)) {}
        });
        idle_switches().await
    });
    let mut failures = Vec::new();
    check("XPUB waiting for a subscription", n, &mut failures);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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

fn footprint_options() -> SocketOptions {
    SocketOptions::default().with_buffer_sizes(4096, 4096)
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

    let (port_tx, port_rx) = mpsc::channel::<u16>();
    let (server_ready_tx, server_ready_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    // Server: accept N connections and hold every ROUTER socket alive, idle.
    let server = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();

            let mut held = Vec::with_capacity(n);
            for _ in 0..n {
                let (stream, _) = listener.accept().await.unwrap();
                let router = RouterSocket::from_tcp_with_options(stream, footprint_options())
                    .await
                    .unwrap();
                held.push(router);
            }
            server_ready_tx.send(()).unwrap();
            // Keep the sockets alive and idle until released.
            release_rx.recv().unwrap();
            drop(held);
        });
    });

    let port = port_rx.recv().unwrap();

    // Client: open N connections and hold every DEALER socket alive, idle.
    let (client_ready_tx, client_ready_rx) = mpsc::channel::<()>();
    let (client_release_tx, client_release_rx) = mpsc::channel::<()>();
    let client = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut held = Vec::with_capacity(n);
            for _ in 0..n {
                let dealer = DealerSocket::connect_with_options(
                    &format!("127.0.0.1:{port}"),
                    footprint_options(),
                )
                .await
                .unwrap();
                held.push(dealer);
            }
            client_ready_tx.send(()).unwrap();
            client_release_rx.recv().unwrap();
            drop(held);
        });
    });

    server_ready_rx.recv().unwrap();
    client_ready_rx.recv().unwrap();

    let rss_after = read_rss_bytes();

    match (rss_before, rss_after) {
        (Some(before), Some(after)) => {
            let delta = after.saturating_sub(before);
            let per_conn = delta as f64 / n as f64;
            // Each logical connection is held on both ends (DEALER + ROUTER) in
            // this process, so `per_conn` is the cost of one connection's two
            // socket endpoints combined.
            println!(
                "[C7 idle footprint] connections={n} rss_before={before} bytes \
                 rss_after={after} bytes delta={delta} bytes \
                 per_connection(both_ends)={per_conn:.0} bytes"
            );
        }
        _ => {
            println!(
                "[C7 idle footprint] RSS measurement unavailable on this platform; \
                 established and held {n} idle connections successfully"
            );
        }
    }

    // Release both sides and join.
    release_tx.send(()).unwrap();
    client_release_tx.send(()).unwrap();
    server.join().unwrap();
    client.join().unwrap();
}
