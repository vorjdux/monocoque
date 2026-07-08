//! Per-socket resident-memory frugality gate.
//!
//! `fanin_rss_bound` guards peak RSS under a *fan-in load*; this guards the
//! *idle* per-socket footprint. It stands up `PAIRS` connected but silent
//! PUSH/PULL pairs, holds them all alive, and asserts the peak resident-set
//! growth stays under `PAIRS * MAX_GROWTH_PER_PAIR_KB`.
//!
//! It exists to reject a change that quietly inflates per-socket resident bytes:
//! an eagerly allocated read slab (the arena removal deliberately keeps the read
//! buffer lazy so an idle socket holds none), a larger resident write buffer
//! (why the 96 KiB coalesce window was kept out of this branch), or per-socket
//! state that grows without bound.
//!
//! Measured VmHWM growth for 200 idle pairs (400 sockets):
//!   compio ~4.0 MB, tokio ~4.0 MB, smol ~4.1 MB  → ~20 KiB per pair.
//! The bound is 96 KiB per pair: comfortably above the measured cost on every
//! backend (~4.8x headroom), and it would be blown through by a 64 KiB eager
//! read slab per socket (which would add ~128 KiB per pair).
//!
//! Linux-only: reads `VmHWM` (peak resident set) from `/proc/self/status`.

#![cfg(target_os = "linux")]

use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;

const PAIRS: usize = 200;
/// Per-pair peak-RSS growth ceiling. Measured cost is ~26-32 KiB per pair; the
/// bound is set at 96 KiB, above the measured value on every backend yet below
/// what a single eager 64 KiB read slab per socket (~128 KiB/pair) would add.
const MAX_GROWTH_PER_PAIR_KB: u64 = 96;

/// Peak resident set size in KiB, from `/proc/self/status` (`VmHWM`).
fn vmhwm_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .expect("parse VmHWM");
        }
    }
    panic!("VmHWM not found in /proc/self/status");
}

#[test]
fn idle_connected_pairs_stay_under_per_socket_footprint_bound() {
    let (port_tx, port_rx) = mpsc::channel::<u16>();
    let (base_tx, base_rx) = mpsc::channel::<u64>();
    let (srv_ready_tx, srv_ready_rx) = mpsc::channel::<()>();
    let (cli_ready_tx, cli_ready_rx) = mpsc::channel::<()>();
    let (srv_done_tx, srv_done_rx) = mpsc::channel::<()>();
    let (cli_done_tx, cli_done_rx) = mpsc::channel::<()>();

    // Server: accept PAIRS PULL ends and hold them idle until released.
    let server = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();

            // Baseline before any socket exists, so growth reflects sockets only.
            base_tx.send(vmhwm_kb()).unwrap();

            let mut pulls = Vec::with_capacity(PAIRS);
            for _ in 0..PAIRS {
                let (stream, _) = listener.accept().await.unwrap();
                pulls.push(
                    PullSocket::from_tcp_with_options(stream, SocketOptions::default())
                        .await
                        .unwrap(),
                );
            }
            srv_ready_tx.send(()).unwrap();
            srv_done_rx.recv().unwrap(); // hold every PULL end alive
            drop(pulls);
        });
    });

    let port = port_rx.recv().unwrap();
    let baseline = base_rx.recv().unwrap();

    // Client: connect PAIRS PUSH ends and hold them idle until released.
    let client = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut pushes = Vec::with_capacity(PAIRS);
            for _ in 0..PAIRS {
                pushes.push(
                    PushSocket::connect_with_options(("127.0.0.1", port), SocketOptions::default())
                        .await
                        .unwrap(),
                );
            }
            cli_ready_tx.send(()).unwrap();
            cli_done_rx.recv().unwrap(); // hold every PUSH end alive
            drop(pushes);
        });
    });

    // Wait until both ends are fully established and still held alive.
    srv_ready_rx.recv().unwrap();
    cli_ready_rx.recv().unwrap();

    let peak = vmhwm_kb();

    // Release both ends and join.
    srv_done_tx.send(()).unwrap();
    cli_done_tx.send(()).unwrap();
    server.join().unwrap();
    client.join().unwrap();

    let growth = peak.saturating_sub(baseline);
    let bound = PAIRS as u64 * MAX_GROWTH_PER_PAIR_KB;
    assert!(
        growth < bound,
        "idle-socket peak RSS growth {growth} KiB exceeded bound {bound} KiB for \
         {PAIRS} connected pairs (baseline {baseline} KiB, peak {peak} KiB, \
         {} KiB/pair vs ceiling {MAX_GROWTH_PER_PAIR_KB}). A per-socket resident \
         buffer grew: check for an eager read slab or a larger write buffer.",
        growth / PAIRS as u64,
    );
}
