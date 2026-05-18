//! High-water-mark (HWM) backpressure tests for PUB and DEALER sockets.
//!
//! PUB HWM: worker channels are bounded by `send_hwm`. When a worker is
//! blocked writing to a slow subscriber, its channel fills up and subsequent
//! `send()` calls increment the drop counter rather than blocking the caller.
//!
//! DEALER HWM: `send_buffered()` enforces `send_hwm` via `WouldBlock`.

use bytes::Bytes;
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::dealer::DealerSocket;
use monocoque_zmtp::publisher::PubSocket as InternalPub;
use monocoque_zmtp::subscriber::SubSocket;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ─────────────────────────────────────────────────────────────────────────────
// PUB drop counter starts at zero
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_pub_drop_count_starts_at_zero() {
    let pub_sock = InternalPub::with_workers(1);
    assert_eq!(pub_sock.drop_count(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// PUB HWM: slow subscriber causes worker channel to fill → drops counted
// ─────────────────────────────────────────────────────────────────────────────

/// A subscriber connects but never calls recv().  The publisher floods messages.
/// Eventually the worker's TCP write buffer fills, blocking it inside
/// `send_message_to_stream`, which prevents it from draining its own channel.
/// With a small channel HWM, `try_send` starts failing and `drop_count` rises.
#[test]
fn test_pub_hwm_drops_with_slow_subscriber() {
    const HWM: usize = 4; // tiny channel so it fills up fast
    const MSGS: usize = 5_000;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (drop_tx, drop_rx) = mpsc::channel::<u64>();

    // Publisher thread
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let opts = SocketOptions::default().with_send_hwm(HWM);
                let mut pub_sock = InternalPub::with_workers_opts(1, opts);
                pub_sock.accept_subscriber(&listener).await.unwrap();

                // Brief pause so subscriber's subscription bytes are processed.
                std::thread::sleep(Duration::from_millis(30));

                for i in 0..MSGS {
                    // Ignore errors — the point is to flood the worker channel.
                    let _ = pub_sock
                        .send(vec![Bytes::new(), Bytes::from(format!("{}", i))])
                        .await;
                }

                drop_tx.send(pub_sock.drop_count()).unwrap();
            });
    });

    let pub_addr = addr_rx.recv().unwrap();

    // Subscriber: connect and subscribe to everything, but never recv().
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let stream = compio::net::TcpStream::connect(pub_addr).await.unwrap();
                let mut sub = SubSocket::from_tcp(stream).await.unwrap();
                sub.subscribe(Bytes::new()).await.unwrap(); // subscribe-all
                                                            // Hold the connection open without reading.
                std::thread::sleep(Duration::from_secs(10));
            });
    });

    let drops = drop_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("publisher thread did not finish");

    assert!(
        drops > 0,
        "Expected some dropped messages with HWM={} and a slow subscriber, got 0",
        HWM
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// DEALER send_buffered HWM → WouldBlock
// ─────────────────────────────────────────────────────────────────────────────

/// `send_buffered()` enforces `send_hwm`. Once the buffer holds `send_hwm`
/// messages, the next call returns `WouldBlock`.
#[test]
fn test_dealer_send_buffered_hwm_returns_would_block() {
    const HWM: usize = 5;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    // Server: accept and hold open.
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let router = monocoque_zmtp::router::RouterSocket::from_tcp(stream)
                    .await
                    .unwrap();
                // Keep alive while client tests HWM.
                std::thread::sleep(Duration::from_secs(5));
                drop(router);
            });
    });

    let addr = addr_rx.recv().unwrap();

    compio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let opts = SocketOptions::default().with_send_hwm(HWM);
            let mut dealer = DealerSocket::connect_with_options(addr, opts)
                .await
                .unwrap();

            // Fill the buffer up to HWM.
            for _ in 0..HWM {
                dealer
                    .send_buffered(vec![Bytes::from("x")])
                    .expect("should succeed below HWM");
            }

            // The (HWM+1)-th call must return WouldBlock.
            let err = dealer
                .send_buffered(vec![Bytes::from("overflow")])
                .expect_err("expected WouldBlock at HWM");

            assert_eq!(
                err.kind(),
                std::io::ErrorKind::WouldBlock,
                "expected WouldBlock, got {:?}",
                err.kind()
            );
        });
}

// ─────────────────────────────────────────────────────────────────────────────
// No OOM / deadlock under high-volume send with moderate HWM
// ─────────────────────────────────────────────────────────────────────────────

/// Stress test: 3 concurrent DEALER sockets each send 10 000 messages with
/// HWM = 100. They must finish without deadlock, OOM, or panicking.
#[test]
fn test_dealer_hwm_stress_no_deadlock() {
    const N_CLIENTS: usize = 3;
    const MSGS: usize = 10_000;
    const HWM: usize = 100;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    // Server: drain messages from all clients.
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut tasks = Vec::new();
                for _ in 0..N_CLIENTS {
                    let (stream, _) = listener.accept().await.unwrap();
                    let task = compio::runtime::spawn(async move {
                        let mut router = monocoque_zmtp::router::RouterSocket::from_tcp(stream)
                            .await
                            .unwrap();
                        loop {
                            match router.recv().await {
                                Ok(Some(_)) => {}
                                _ => break,
                            }
                        }
                    });
                    tasks.push(task);
                }
                for t in tasks {
                    t.await;
                }
            });
    });

    let addr = addr_rx.recv().unwrap();

    let handles: Vec<_> = (0..N_CLIENTS)
        .map(|_| {
            thread::spawn(move || {
                compio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(async move {
                        let opts = SocketOptions::default().with_send_hwm(HWM);
                        let mut dealer = DealerSocket::connect_with_options(addr, opts)
                            .await
                            .unwrap();

                        for i in 0..MSGS {
                            // Use direct send() (unbuffered) to avoid HWM on the
                            // buffered path; the point is network-layer throughput.
                            dealer
                                .send(vec![Bytes::new(), Bytes::from(format!("{}", i))])
                                .await
                                .unwrap();
                        }
                    })
            })
        })
        .collect();

    for h in handles {
        h.join().expect("client thread panicked");
    }
}
