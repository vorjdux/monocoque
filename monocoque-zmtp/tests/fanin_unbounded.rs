//! A PULL with an unbounded receive HWM fed by several fast PUSH peers keeps
//! delivering. On a readiness runtime with no task budget (smol), a reader
//! whose peer keeps sending never sees `WouldBlock`, and with no HWM nothing
//! else stops it; the receiver used to spend each receive running those
//! readers and fell to about a thousand messages a second.

use monocoque_core::msg::Msg;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::LocalRuntime;
use monocoque_zmtp::sock::kinds::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const PEERS: usize = 4;
const MESSAGES: u64 = 500_000;
/// Several times what the fixed receiver needs (a fraction of a second);
/// the starved one managed about 1 000 messages a second.
const BUDGET: Duration = Duration::from_secs(20);

#[test]
fn unbounded_pull_keeps_up_with_four_streaming_peers() {
    let (tx, rx) = std::sync::mpsc::channel();
    let sink = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pull = PullSocket::with_options(
                SocketOptions::default()
                    .with_recv_hwm(0)
                    .with_recv_timeout(Duration::from_secs(5)),
            );
            tx.send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            pull.recv().await.unwrap();
            let start = Instant::now();
            for i in 0..MESSAGES {
                pull.recv().await.unwrap();
                assert!(
                    start.elapsed() < BUDGET,
                    "only {i} of {MESSAGES} messages in {BUDGET:?}: the receiver is starved"
                );
            }
        });
    });
    let ep = rx.recv().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let pushers: Vec<_> = (0..PEERS)
        .map(|_| {
            let (ep, stop) = (ep.clone(), Arc::clone(&stop));
            thread::spawn(move || {
                LocalRuntime::new().unwrap().block_on(async move {
                    let mut push = PushSocket::with_options(
                        SocketOptions::default()
                            .with_write_coalescing(true)
                            .with_send_timeout(Duration::from_millis(500))
                            .with_linger(Some(Duration::ZERO)),
                    );
                    push.connect(&ep).await.unwrap();
                    let msg = Msg::from(vec![1u8; 64]);
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        if push.send(msg.clone()).await.is_err() {
                            break;
                        }
                        i += 1;
                        if i.is_multiple_of(64) {
                            let flushed = monocoque_core::rt::timeout(
                                Duration::from_millis(500),
                                push.flush(),
                            )
                            .await;
                            if !matches!(flushed, Ok(Ok(()))) {
                                break;
                            }
                        }
                    }
                });
            })
        })
        .collect();
    let result = sink.join();
    stop.store(true, Ordering::Relaxed);
    for p in pushers {
        let _ = p.join();
    }
    result.unwrap();
}
