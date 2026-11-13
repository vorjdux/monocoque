//! Per-socket resident-memory frugality gate.
//!
//! `fanin_rss_bound` guards peak RSS under a *fan-in load*; this guards the
//! *idle* per-socket footprint. It stands up `PAIRS` connected but silent
//! PUSH/PULL pairs, holds them all alive, and asserts the peak resident-set
//! growth stays under `PAIRS * MAX_GROWTH_PER_PAIR_KB`.
//!
//! It exists to reject a change that quietly inflates per-socket resident bytes:
//! an eagerly allocated read buffer, a larger resident write buffer, or
//! per-socket state that grows without bound.
//!
//! The bound is 96 KiB per pair, which a single eager 64 KiB read buffer per
//! socket (~128 KiB per pair) would exceed.
//!
//! Linux-only: reads `VmHWM` (peak resident set) from `/proc/self/status`.

#![cfg(target_os = "linux")]
// The socket vectors exist only to keep the connections open.
#![allow(clippy::collection_is_never_read)]

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{PullSocket, PushSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const PAIRS: usize = 200;
/// Per-pair peak-RSS growth ceiling.
const MAX_GROWTH_PER_PAIR_KB: u64 = 96;

/// Peak resident set size in KiB, from `/proc/self/status` (`VmHWM`).
fn vmhwm_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmHWM in /proc/self/status")
}

#[test]
fn idle_connected_pairs_stay_under_per_socket_footprint_bound() {
    let (eps_tx, eps_rx) = mpsc::channel::<Vec<String>>();
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let (cli_done_tx, cli_done_rx) = mpsc::channel::<()>();

    let baseline = vmhwm_kb();

    let server = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pulls = Vec::with_capacity(PAIRS);
            let mut eps = Vec::with_capacity(PAIRS);
            for _ in 0..PAIRS {
                let mut pull = PullSocket::new();
                eps.push(pull.bind("tcp://127.0.0.1:0").await.unwrap());
                pulls.push(pull);
            }
            eps_tx.send(eps).unwrap();
            while pulls.iter().any(|p| p.peer_count() == 0) {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
            ready_tx.send(()).unwrap();
            // Hold every PULL end alive while the peak is read.
            while done_rx.try_recv().is_err() {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
        });
    });

    let eps = eps_rx.recv().unwrap();
    let client = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pushes = Vec::with_capacity(PAIRS);
            for ep in &eps {
                let mut push = PushSocket::new();
                push.connect(ep).await.unwrap();
                pushes.push(push);
            }
            while cli_done_rx.try_recv().is_err() {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
        });
    });

    ready_rx.recv().unwrap();
    let peak = vmhwm_kb();
    done_tx.send(()).unwrap();
    cli_done_tx.send(()).unwrap();
    server.join().unwrap();
    client.join().unwrap();

    let growth = peak.saturating_sub(baseline);
    let bound = PAIRS as u64 * MAX_GROWTH_PER_PAIR_KB;
    println!(
        "[footprint] {growth} KiB for {PAIRS} pairs ({} KiB/pair)",
        growth / PAIRS as u64
    );
    assert!(
        growth < bound,
        "idle-socket peak RSS growth {growth} KiB exceeded bound {bound} KiB for \
         {PAIRS} connected pairs ({} KiB/pair vs ceiling {MAX_GROWTH_PER_PAIR_KB}). \
         A per-socket resident buffer grew.",
        growth / PAIRS as u64,
    );
}
