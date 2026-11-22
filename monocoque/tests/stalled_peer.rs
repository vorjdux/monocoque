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

/// A peer that stopped reading must be marked whatever the send timeout,
/// or every send to it waits out the whole timeout until its HWM fills (100 ms
/// a send, 100 s for this run). A timeout shorter than `write_stall` never
/// sees a whole window in one send, so the watch has to last across sends.
#[test]
fn a_short_send_timeout_still_marks_a_stalled_peer() {
    for ms in [5, 20, 40, 50, 100] {
        let timeout = Duration::from_millis(ms);
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

/// Answer a ZMTP 3 NULL handshake as a PULL socket.
fn pull_handshake(s: &mut std::net::TcpStream) {
    use std::io::{Read, Write};
    let mut greeting = [0u8; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3;
    greeting[12..16].copy_from_slice(b"NULL");
    s.write_all(&greeting).unwrap();
    let mut peer = [0u8; 64];
    s.read_exact(&mut peer).unwrap();
    let mut ready = vec![5u8];
    ready.extend_from_slice(b"READY");
    ready.push(11);
    ready.extend_from_slice(b"Socket-Type");
    ready.extend_from_slice(&4u32.to_be_bytes());
    ready.extend_from_slice(b"PULL");
    s.write_all(&[0x04, u8::try_from(ready.len()).unwrap()])
        .unwrap();
    s.write_all(&ready).unwrap();
}

/// A `send_timeout` below `write_stall` shortens the wait a peer's progress is
/// judged over, not what it has to take per window. A reader taking a steady
/// 6 MB/s, 12 KiB per 2 ms, never stopped reading and must not be reported
/// stalled; held to a whole window's 16 KiB in each 2 ms wait, it was.
#[test]
fn a_short_send_timeout_does_not_report_a_steady_reader_stalled() {
    use monocoque::zmq::{PushSocket, SocketEvent};
    use std::io::Read;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ep = format!("tcp://{}", listener.local_addr().unwrap());
    let reader = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        pull_handshake(&mut s);
        let mut buf = vec![0u8; 12 * 1024];
        let mut got = 0usize;
        // About 6 MB/s: 12 KiB, then 2 ms, until the sender closes.
        loop {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => return got,
                Ok(n) => got += n,
            }
            thread::sleep(Duration::from_millis(2));
        }
    });

    let events = LocalRuntime::new().unwrap().block_on(async move {
        // The window is longer than the default so that the reader's thread,
        // descheduled while this binary's other tests run, cannot pause long
        // enough to count as a reader that really stopped. What is tested is
        // that 2 ms waits are not judged on their own.
        let mut push = PushSocket::with_options(
            SocketOptions::default()
                .with_send_timeout(Duration::from_millis(2))
                .with_write_stall(Some(Duration::from_millis(200))),
        );
        let monitor = push.monitor();
        push.connect(&ep).await.unwrap();
        let body = Msg::from(vec![1u8; 1024]);
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            let _ = push.send(body.clone()).await;
        }
        drop(push);
        monitor.drain().collect::<Vec<_>>()
    });
    let got = reader.join().unwrap();
    assert!(got > 1 << 20, "the reader took only {got} bytes");
    let stalled: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, SocketEvent::PeerStalled(_)))
        .collect();
    assert!(
        stalled.is_empty(),
        "a steady reader was reported stalled: {stalled:?}"
    );
}
