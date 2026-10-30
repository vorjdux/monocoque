//! PUB fan-out to many TCP subscribers keeps every queue draining: a
//! publisher that waits on `flush` when queues are full must make progress.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, PubSocket, SubSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const SUBS: usize = 100;
const COUNT: usize = 20_000;

#[test]
fn fanout_to_many_tcp_subscribers_drains() {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let publisher = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut publisher = PubSocket::new();
            ep_tx
                .send(publisher.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            let payload = Msg::from(vec![7u8; 64]);
            while publisher.send(payload.clone()).await.unwrap() < SUBS {
                monocoque::rt::sleep(Duration::from_millis(1)).await;
            }
            // PUB never waits on a full subscriber, so the loop yields to the
            // runtime once per burst to let the writers run.
            while stop_rx.try_recv().is_err() {
                for _ in 0..100 {
                    publisher.send(payload.clone()).await.unwrap();
                }
                monocoque::rt::sleep(Duration::from_millis(1)).await;
            }
        });
    });
    let ep = ep_rx.recv().unwrap();
    // One subscriber blocks its thread between receives, so its queue at the
    // publisher stays full; it must not stall the others.
    let slow_ep = ep.clone();
    let (slow_stop_tx, slow_stop_rx) = mpsc::channel::<()>();
    let slow = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut sub = SubSocket::new();
            sub.subscribe(b"").await.unwrap();
            sub.connect(&slow_ep).await.unwrap();
            while slow_stop_rx.try_recv().is_err() {
                thread::sleep(Duration::from_millis(5));
                if sub.recv().await.is_err() {
                    break;
                }
            }
        });
    });
    let subs: Vec<_> = (0..SUBS - 1)
        .map(|_| {
            let ep = ep.clone();
            thread::spawn(move || {
                LocalRuntime::new().unwrap().block_on(async move {
                    let mut sub = SubSocket::with_options(
                        monocoque::zmq::SocketOptions::default()
                            .with_recv_timeout(Duration::from_secs(10)),
                    );
                    sub.subscribe(b"").await.unwrap();
                    sub.connect(&ep).await.unwrap();
                    for _ in 0..COUNT {
                        sub.recv().await.expect("subscriber starved");
                    }
                });
            })
        })
        .collect();
    for s in subs {
        s.join().unwrap();
    }
    stop_tx.send(()).unwrap();
    publisher.join().unwrap();
    slow_stop_tx.send(()).unwrap();
    slow.join().unwrap();
}
