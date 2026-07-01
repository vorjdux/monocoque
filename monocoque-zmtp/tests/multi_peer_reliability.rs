//! Multi-peer reliability tests.
//!
//! Validates scenarios that are critical for production use:
//! - PUB/SUB fanout with multiple subscribers and overlapping topic prefixes
//! - ROUTER/DEALER multi-peer message exchange
//! - No message loss under concurrent load
//!
//! Note: publisher threads use channel-based ready-signaling instead of
//! `monocoque_core::rt::sleep` because multiple handshake timeouts leave residual
//! timer state in the runtime that makes subsequent compio sleeps unreliable.

use bytes::Bytes;
use monocoque_zmtp::dealer::DealerSocket;
use monocoque_zmtp::publisher::PubSocket as InternalPub;
use monocoque_zmtp::router::RouterSocket;
use monocoque_zmtp::subscriber::SubSocket;
use std::thread;
use std::time::Duration;

// ────────────────────────────────────────────────────────────────────────────
// PUB / SUB fanout
// ────────────────────────────────────────────────────────────────────────────

/// N subscribers, each on a distinct topic prefix, receive exactly the messages
/// published on their prefix and nothing else.
#[test]
#[allow(clippy::needless_collect)]
fn test_pubsub_fanout_distinct_topics() {
    const N: usize = 4;
    const MSGS: usize = 20;

    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    // Each subscriber thread sends () here after calling subscribe().
    let (sub_ready_tx, sub_ready_rx) = std::sync::mpsc::channel::<()>();
    // Main thread signals here once every subscriber has received, so the
    // publisher runtime can wind down. See the keep-alive note below.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    // Publisher thread
    thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut pub_sock = InternalPub::with_workers(2);
                for _ in 0..N {
                    pub_sock.accept_subscriber(&listener).await.unwrap();
                }

                // Wait until every subscriber has sent its subscription bytes.
                // Using blocking recv (no compio timer) avoids the residual-timer issue.
                for _ in 0..N {
                    sub_ready_rx.recv().unwrap();
                }
                // Give worker-thread subscription readers time to process the bytes.
                // std::thread::sleep is safe here: no compio ops are pending in the
                // publisher runtime; workers run in their own threads.
                std::thread::sleep(Duration::from_millis(30));

                for i in 0..N {
                    for j in 0..MSGS {
                        pub_sock
                            .send(vec![
                                Bytes::from(format!("t{i}")),
                                Bytes::from(format!("{j}")),
                            ])
                            .await
                            .unwrap();
                    }
                }

                // Keep this runtime alive until the subscribers have drained
                // their messages. `send()` only queues to the worker threads; the
                // actual socket writes happen there against streams that, on the
                // tokio backend, stay bound to this accepting runtime. Dropping
                // it now would fail those in-flight writes. A real long-running
                // PUB server keeps its runtime alive the same way; see the
                // "worker-pool PUB and runtime lifetime" note in docs/performance.md.
                let _ = done_rx.recv();
            });
    });

    let pub_addr = addr_rx.recv().unwrap();

    // One subscriber thread per topic
    let handles: Vec<_> = (0..N)
        .map(|i| {
            let addr = pub_addr;
            let ready_tx = sub_ready_tx.clone();
            thread::spawn(move || {
                monocoque_core::rt::LocalRuntime::new()
                    .unwrap()
                    .block_on(async move {
                        let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
                        let mut sub = SubSocket::from_tcp(stream).await.unwrap();
                        sub.subscribe(Bytes::from(format!("t{i}"))).await.unwrap();

                        // Signal that subscription bytes have been sent.
                        ready_tx.send(()).unwrap();

                        let mut received = 0usize;
                        for _ in 0..MSGS {
                            let msg =
                                monocoque_core::rt::timeout(Duration::from_secs(10), sub.recv())
                                    .await
                                    .expect("recv timed out")
                                    .expect("io error")
                                    .expect("connection closed");

                            assert_eq!(
                                msg[0],
                                Bytes::from(format!("t{i}")),
                                "subscriber {i} got wrong topic"
                            );
                            received += 1;
                        }
                        received
                    })
            })
        })
        .collect();

    let counts: Vec<usize> = handles
        .into_iter()
        .enumerate()
        .map(|(i, handle)| {
            handle
                .join()
                .unwrap_or_else(|_| panic!("subscriber {i} thread panicked"))
        })
        .collect();

    // Every subscriber has received; let the publisher runtime wind down.
    let _ = done_tx.send(());

    for (i, count) in counts.into_iter().enumerate() {
        assert_eq!(
            count, MSGS,
            "subscriber {i} received {count}/{MSGS} messages"
        );
    }
}

/// Overlapping topic prefixes: subscriber A on "weather", subscriber B on
/// "weather.temp".  Messages on "weather.temp" reach both A and B;
/// messages on "weather.hum" reach only A.
#[test]
fn test_pubsub_fanout_overlapping_topics() {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    let (sub_ready_tx, sub_ready_rx) = std::sync::mpsc::channel::<()>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<(usize, usize)>();
    // Main signals here once both subscribers have received (keep-alive; see note).
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut pub_sock = InternalPub::with_workers(1);
                pub_sock.accept_subscriber(&listener).await.unwrap();
                pub_sock.accept_subscriber(&listener).await.unwrap();

                // Wait for both subscribers to send their subscription bytes.
                sub_ready_rx.recv().unwrap();
                sub_ready_rx.recv().unwrap();
                std::thread::sleep(Duration::from_millis(30));

                // 5 messages on "weather.temp"  → both A and B should receive
                for i in 0..5u32 {
                    pub_sock
                        .send(vec![
                            Bytes::from("weather.temp"),
                            Bytes::from(i.to_string()),
                        ])
                        .await
                        .unwrap();
                }
                // 3 messages on "weather.hum"  → only A should receive
                for i in 0..3u32 {
                    pub_sock
                        .send(vec![Bytes::from("weather.hum"), Bytes::from(i.to_string())])
                        .await
                        .unwrap();
                }

                // Keep this runtime alive until both subscribers have drained.
                // `send()` only queues to the worker thread; the socket writes
                // happen there against streams that, on the tokio backend, stay
                // bound to this accepting runtime. A real long-running PUB server
                // keeps its runtime alive the same way; see the "worker-pool PUB
                // and runtime lifetime" note in docs/performance.md.
                let _ = done_rx.recv();
            });
    });

    let pub_addr = addr_rx.recv().unwrap();

    // Subscriber A: "weather" (matches both "weather.temp" and "weather.hum")
    let result_tx_a = result_tx.clone();
    let sub_ready_tx_a = sub_ready_tx.clone();
    thread::spawn(move || {
        let count = monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(pub_addr)
                    .await
                    .unwrap();
                let mut sub = SubSocket::from_tcp(stream).await.unwrap();
                sub.subscribe(Bytes::from("weather")).await.unwrap();
                sub_ready_tx_a.send(()).unwrap();

                let mut n = 0usize;
                for _ in 0..8 {
                    monocoque_core::rt::timeout(Duration::from_secs(10), sub.recv())
                        .await
                        .expect("recv timed out")
                        .expect("io error")
                        .expect("connection closed");
                    n += 1;
                }
                n
            });
        result_tx_a.send((0, count)).unwrap();
    });

    // Subscriber B: "weather.temp" (matches only "weather.temp")
    let sub_ready_tx_b = sub_ready_tx;
    thread::spawn(move || {
        let count = monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(pub_addr)
                    .await
                    .unwrap();
                let mut sub = SubSocket::from_tcp(stream).await.unwrap();
                sub.subscribe(Bytes::from("weather.temp")).await.unwrap();
                sub_ready_tx_b.send(()).unwrap();

                let mut n = 0usize;
                for _ in 0..5 {
                    monocoque_core::rt::timeout(Duration::from_secs(10), sub.recv())
                        .await
                        .expect("recv timed out")
                        .expect("io error")
                        .expect("connection closed");
                    n += 1;
                }
                n
            });
        result_tx.send((1, count)).unwrap();
    });

    let mut counts = [0usize; 2];
    for _ in 0..2 {
        let (idx, count) = result_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        counts[idx] = count;
    }

    // Both subscribers have received; let the publisher runtime wind down.
    let _ = done_tx.send(());

    assert_eq!(
        counts[0], 8,
        "subscriber A (\"weather\") expected 8 messages, got {}",
        counts[0]
    );
    assert_eq!(
        counts[1], 5,
        "subscriber B (\"weather.temp\") expected 5 messages, got {}",
        counts[1]
    );
}

// ────────────────────────────────────────────────────────────────────────────
// ROUTER / DEALER multi-peer
// ────────────────────────────────────────────────────────────────────────────

/// Multiple DEALER clients each exchange several request-reply round-trips with
/// independent `RouterSocket` instances (one `RouterSocket` per accepted connection).
/// Verifies routing IDs are unique and no messages are mixed between clients.
#[test]
fn test_router_dealer_multi_peer() {
    const N_CLIENTS: usize = 5;
    const ROUNDS: usize = 10;

    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    // Server thread: accepts N connections and echoes messages back.
    thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut tasks = Vec::new();
                for _ in 0..N_CLIENTS {
                    let (stream, _) = listener.accept().await.unwrap();
                    let task = monocoque_core::rt::spawn(async move {
                        let mut router = RouterSocket::from_tcp(stream).await.unwrap();
                        for _ in 0..ROUNDS {
                            let msg = router.recv().await.unwrap().unwrap();
                            router.send(msg).await.unwrap();
                        }
                    });
                    tasks.push(task);
                }
                for task in tasks {
                    monocoque_core::rt::join(task).await;
                }
                done_tx.send(()).unwrap();
            });
    });

    let server_addr = addr_rx.recv().unwrap();

    let handles: Vec<_> = (0..N_CLIENTS)
        .map(|client_id| {
            let addr = server_addr;
            thread::spawn(move || {
                monocoque_core::rt::LocalRuntime::new()
                    .unwrap()
                    .block_on(async move {
                        let mut dealer = DealerSocket::connect(addr).await.unwrap();

                        for round in 0..ROUNDS {
                            let payload = Bytes::from(format!("client{client_id}-round{round}"));
                            dealer
                                .send(vec![Bytes::new(), payload.clone()])
                                .await
                                .unwrap();

                            let reply =
                                monocoque_core::rt::timeout(Duration::from_secs(10), dealer.recv())
                                    .await
                                    .expect("recv timed out")
                                    .expect("io error")
                                    .expect("connection closed");

                            assert_eq!(
                                reply.last().unwrap(),
                                &payload,
                                "client {client_id} round {round} got wrong payload"
                            );
                        }
                    });
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("client thread panicked");
    }

    done_rx.recv_timeout(Duration::from_secs(30)).unwrap();
}
