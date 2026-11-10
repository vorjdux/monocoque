//! A peer that stops reading must not stall its socket's other peers.
//!
//! The stalled peer's kernel buffer fills, so its writes stop completing. A
//! send to it must still return (a non-mandatory ROUTER drops at the HWM, as
//! libzmq does), and a healthy peer on the same socket keeps receiving.

use monocoque::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, Msg, RouterSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const SENDS: usize = 20_000;
/// Far above what the sends take when none waits on the stalled peer.
const BUDGET: Duration = Duration::from_secs(10);

/// Send `SENDS` messages alternately to a peer that stopped reading and to one
/// that reads, with `send_timeout` on the ROUTER. Returns how long the sends
/// took and how many the reading peer got.
fn run(send_timeout: Option<Duration>) -> (Duration, usize) {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (done_tx, done_rx) = mpsc::channel::<Duration>();
    thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut options = SocketOptions::default().with_recv_timeout(Duration::from_secs(10));
            if let Some(t) = send_timeout {
                options = options.with_send_timeout(t);
            }
            let mut router = RouterSocket::with_options(options);
            ep_tx
                .send(router.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            // Both peers say hello first, so both are routable.
            for _ in 0..2 {
                router.recv().await.unwrap();
            }
            let body = Bytes::from(vec![7u8; 1024]);
            let start = Instant::now();
            for i in 0..SENDS {
                let to: &'static [u8] = if i % 2 == 0 { b"stalled" } else { b"reader" };
                // A send that runs out of time has still queued its message.
                let _ = router
                    .send(Msg::from([Bytes::from_static(to), body.clone()]))
                    .await;
            }
            done_tx.send(start.elapsed()).unwrap();
        });
    });
    let ep = ep_rx.recv().unwrap();

    // Says hello, then parks its thread with the socket open: its socket
    // buffer fills and stays full.
    let (park_tx, park_rx) = mpsc::channel::<()>();
    let stalled_ep = ep.clone();
    thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut d = DealerSocket::with_options(
                SocketOptions::default().with_routing_id(Bytes::from_static(b"stalled")),
            );
            d.connect(&stalled_ep).await.unwrap();
            d.send("hello").await.unwrap();
            let _ = park_rx.recv();
        });
    });

    let (got_tx, got_rx) = mpsc::channel::<usize>();
    thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut d = DealerSocket::with_options(
                SocketOptions::default()
                    .with_routing_id(Bytes::from_static(b"reader"))
                    .with_recv_timeout(Duration::from_secs(5)),
            );
            d.connect(&ep).await.unwrap();
            d.send("hello").await.unwrap();
            let mut got = 0;
            while got < SENDS / 2 && d.recv().await.is_ok() {
                got += 1;
            }
            got_tx.send(got).unwrap();
        });
    });

    let elapsed = done_rx
        .recv_timeout(BUDGET)
        .expect("ROUTER sends stalled behind the peer that stopped reading");
    let got = got_rx
        .recv_timeout(BUDGET)
        .expect("the reading peer stalled");
    drop(park_tx);
    (elapsed, got)
}

#[test]
fn router_sends_never_wait_on_a_peer_that_stopped_reading() {
    let (elapsed, got) = run(None);
    assert_eq!(got, SENDS / 2, "the reading peer lost messages");
    assert!(elapsed < BUDGET, "sends took {elapsed:?}");
}

/// With a send timeout of two `write_stall` windows or less there may be no
/// second window to compare progress against. The stalled peer must still be
/// marked, or every send to it waits out the whole timeout until its HWM
/// fills (100 ms a send, 100 s for this run, before the fix).
#[test]
fn a_short_send_timeout_still_marks_a_stalled_peer() {
    for timeout in [Duration::from_millis(50), Duration::from_millis(100)] {
        let (elapsed, got) = run(Some(timeout));
        assert_eq!(
            got,
            SENDS / 2,
            "the reading peer lost messages at {timeout:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "with send_timeout {timeout:?} the sends took {elapsed:?}"
        );
    }
}
