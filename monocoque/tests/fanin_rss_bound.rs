//! Peak-RSS guard for a fan-in sink: one PULL socket merging many PUSH workers.
//!
//! The arrival tests prove every message reaches the sink; this one bounds how
//! much memory the sink holds on the way. The failure it guards against: a sink
//! that lags its readers and retains an unbounded number of messages, each
//! pinning the read buffer it was decoded from. Here every peer's ready queue
//! stops at the receive HWM (backpressuring that worker through the kernel), and
//! a small message is copied inline so it pins no buffer at all.
//!
//! The worst cell: the smallest payload at a worker count high enough that the
//! sink falls behind. A regression of the old kind measured ~66 MB here; the
//! bound is 40 MB.
//!
//! Linux-only: it reads `VmHWM` (peak resident set) from `/proc/self/status`.
#![cfg(target_os = "linux")]

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const WORKERS: usize = 32;
const PAYLOAD: usize = 64;
const TOTAL: usize = 400_000;
const MAX_RSS_GROWTH_KB: u64 = 40 * 1024;

fn vmhwm_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmHWM in /proc/self/status")
}

#[test]
fn fan_in_peak_rss_stays_bounded_with_a_lagging_sink() {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let baseline = vmhwm_kb();

    let sink = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pull = PullSocket::with_options(
                SocketOptions::default().with_recv_timeout(Duration::from_secs(30)),
            );
            ep_tx
                .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            for _ in 0..TOTAL {
                let m = pull.recv().await.unwrap();
                assert_eq!(m.byte_len(), PAYLOAD);
            }
        });
    });

    let endpoint = ep_rx.recv().unwrap();
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let endpoint = endpoint.clone();
            thread::spawn(move || {
                LocalRuntime::new().unwrap().block_on(async move {
                    let mut push = PushSocket::new();
                    push.connect(&endpoint).await.unwrap();
                    let payload = Msg::from(vec![9u8; PAYLOAD]);
                    for _ in 0..TOTAL / WORKERS {
                        push.send(payload.clone()).await.unwrap();
                    }
                    push.close().await.unwrap();
                });
            })
        })
        .collect();

    for w in workers {
        w.join().unwrap();
    }
    sink.join().unwrap();

    let growth = vmhwm_kb().saturating_sub(baseline);
    println!("[fanin_rss_bound] peak RSS growth {growth} KiB");
    assert!(
        growth < MAX_RSS_GROWTH_KB,
        "fan-in sink peak RSS grew {growth} KiB (bound {MAX_RSS_GROWTH_KB} KiB): \
         queued messages are being retained without bound or are pinning buffers"
    );
}
