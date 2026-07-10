//! Peak-RSS regression guard for `PullFanIn`.
//!
//! The correctness tests in `pipeline_fanout_fanin.rs` verify that every message
//! a worker sends reaches the sink. They do **not** verify *how much memory* the
//! sink holds to get them there, and that is exactly where `PullFanIn` regressed
//! once: when the merge channel bounded item *count* but each item was a whole
//! kernel read of unbounded message count, a sink lagging N readers retained an
//! unbounded number of messages, and because every frozen message pins its whole
//! 64 KiB slab page, peak RSS climbed to ~66 MB at 32 workers / 64 B payload. A
//! change that reintroduces per-message page pinning (or drops the per-item cap)
//! passes every arrival test silently; only an RSS assertion catches it.
//!
//! This guard drives the worst cell for the bug: the smallest payload, where the
//! 64 KiB-page-per-message amplification is largest, at a worker count high enough
//! that the single sink falls behind the readers and the queue fills. It asserts
//! peak RSS growth stays under a bound that the buggy code blew through by ~4x.
//!
//! Measured `VmHWM` growth over baseline at 32 workers / 64 B / 400k messages:
//! buggy (unbounded batch item) ~66 MB, fixed (per-item message cap) ~15 MB.
//! The bound is set at 40 MB: comfortably above the fixed cost on the measured
//! backends (compio ~15 MB, tokio ~13 MB) and well below the ~66 MB the regression
//! cost.
//!
//! Linux-only: it reads `VmHWM` (peak resident set) from `/proc/self/status`.

#![cfg(target_os = "linux")]

use bytes::Bytes;
use monocoque::rt::TcpListener;
use monocoque::zmq::{PullFanIn, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;

const WORKERS: usize = 32;
const PAYLOAD: usize = 64;
const TOTAL: usize = 400_000;
/// Peak RSS growth ceiling. Fixed code sits ~15 MB below this; the regression
/// this guards against sat ~26 MB above it.
const MAX_RSS_GROWTH_KB: u64 = 40 * 1024;

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

fn coalescing_opts() -> SocketOptions {
    SocketOptions::default()
        .with_buffer_sizes(16384, 16384)
        .with_write_coalescing(true)
}

/// N PUSH workers blast small messages into one `PullFanIn` while the sink
/// drains; peak RSS growth must stay under the bound.
#[test]
fn pull_fanin_peak_rss_stays_bounded_under_sink_lag() {
    let per_worker = TOTAL / WORKERS;
    let recv_total = per_worker * WORKERS;

    // Baseline before the workload so the assertion measures this test's growth,
    // not fixed process startup cost.
    let baseline = vmhwm_kb();

    let (port_tx, port_rx) = mpsc::channel::<u16>();

    let sink = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            port_tx.send(port).unwrap();

            let mut sink = PullFanIn::accept_workers(&listener, WORKERS, coalescing_opts())
                .await
                .unwrap();

            let mut got = 0usize;
            while got < recv_total {
                match sink.recv().await.unwrap() {
                    Some(_) => got += 1,
                    None => break,
                }
            }
            got
        })
    });

    let port = port_rx.recv().unwrap();
    let payload = Bytes::from(vec![0u8; PAYLOAD]);

    let mut workers = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let payload = payload.clone();
        workers.push(thread::spawn(move || {
            let rt = monocoque::rt::LocalRuntime::new().unwrap();
            rt.block_on(async move {
                let mut push =
                    PushSocket::connect_with_options(("127.0.0.1", port), coalescing_opts())
                        .await
                        .unwrap();
                for _ in 0..per_worker {
                    push.send(vec![payload.clone()]).await.unwrap();
                }
                push.flush().await.unwrap();
            });
        }));
    }

    let received = sink.join().unwrap();
    for w in workers {
        w.join().unwrap();
    }

    let peak = vmhwm_kb();
    let growth = peak.saturating_sub(baseline);

    // Sanity: the workload actually ran to completion (so the RSS number reflects
    // the full fan-in load, not an early bail-out).
    assert_eq!(
        received, recv_total,
        "sink received {received} of {recv_total} messages"
    );

    assert!(
        growth < MAX_RSS_GROWTH_KB,
        "PullFanIn peak RSS growth {growth} KiB exceeded bound {MAX_RSS_GROWTH_KB} KiB \
         at {WORKERS} workers / {PAYLOAD} B / {recv_total} msgs (baseline {baseline} KiB, \
         peak {peak} KiB). This is the unbounded-page-pinning regression the guard exists \
         to catch: a queued message must not retain its whole 64 KiB slab page without bound.",
    );
}
