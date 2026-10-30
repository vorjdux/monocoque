//! Fan-out and fan-in pipelines on multi-peer PUSH and PULL sockets.
//!
//! - Fan-out: one bound PUSH spreads tasks round-robin across N PULL workers.
//! - Fan-in: one bound PULL merges results from N PUSH workers.
//!
//! Every socket runs on its own thread with its own runtime, so peers are truly
//! concurrent. Nothing may be lost or duplicated.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const WORKERS: usize = 4;
const PER_WORKER: usize = 50;
const TOTAL: usize = WORKERS * PER_WORKER;

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(10))
}

#[test]
fn test_fanout_round_robin_reaches_every_worker() {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (go_tx, go_rx) = mpsc::channel::<()>();

    let ventilator = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut push = PushSocket::new();
            ep_tx
                .send(push.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            // Wait for the whole pool so round-robin covers every worker.
            go_rx.recv().unwrap();
            while push.peer_count() < WORKERS {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
            for i in 0..TOTAL {
                push.send(Msg::from(i.to_string())).await.unwrap();
            }
            push.close().await.unwrap();
        });
    });

    let endpoint = ep_rx.recv().unwrap();
    let (got_tx, got_rx) = mpsc::channel::<Vec<usize>>();
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let endpoint = endpoint.clone();
            let got_tx = got_tx.clone();
            thread::spawn(move || {
                LocalRuntime::new().unwrap().block_on(async move {
                    let mut pull = PullSocket::with_options(opts());
                    pull.connect(&endpoint).await.unwrap();
                    let mut mine = Vec::new();
                    for _ in 0..PER_WORKER {
                        let m = pull.recv().await.unwrap();
                        mine.push(
                            std::str::from_utf8(m.first().unwrap())
                                .unwrap()
                                .parse()
                                .unwrap(),
                        );
                    }
                    got_tx.send(mine).unwrap();
                });
            })
        })
        .collect();
    go_tx.send(()).unwrap();

    let mut all = Vec::new();
    for _ in 0..WORKERS {
        let mine = got_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(
            mine.len(),
            PER_WORKER,
            "round-robin gives each worker an equal share"
        );
        all.extend(mine);
    }
    for w in workers {
        w.join().unwrap();
    }
    ventilator.join().unwrap();
    all.sort_unstable();
    assert_eq!(
        all,
        (0..TOTAL).collect::<Vec<_>>(),
        "every task exactly once"
    );
}

#[test]
fn test_fanin_sink_receives_every_result() {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let sink = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pull = PullSocket::with_options(opts());
            ep_tx
                .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            let mut all = Vec::new();
            for _ in 0..TOTAL {
                let m = pull.recv().await.unwrap();
                all.push(std::str::from_utf8(m.first().unwrap()).unwrap().to_string());
            }
            all
        })
    });

    let endpoint = ep_rx.recv().unwrap();
    let workers: Vec<_> = (0..WORKERS)
        .map(|w| {
            let endpoint = endpoint.clone();
            thread::spawn(move || {
                LocalRuntime::new().unwrap().block_on(async move {
                    let mut push = PushSocket::new();
                    push.connect(&endpoint).await.unwrap();
                    for i in 0..PER_WORKER {
                        push.send(Msg::from(format!("{w}-{i}"))).await.unwrap();
                    }
                    push.close().await.unwrap();
                });
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    let mut all = sink.join().unwrap();
    all.sort();
    let mut expected: Vec<String> = (0..WORKERS)
        .flat_map(|w| (0..PER_WORKER).map(move |i| format!("{w}-{i}")))
        .collect();
    expected.sort();
    assert_eq!(all, expected);
}
