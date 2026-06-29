//! Integration tests for SUB socket option-based subscription wiring.
//!
//! Verifies that subscriptions declared via `SocketOptions::with_subscribe` are
//! sent to the PUB peer automatically during socket construction, so callers
//! do not have to call `subscribe()` manually after creation.
//!
//! Test structure mirrors `multi_peer_reliability.rs`:
//!  - a sync channel signals the PUB that subscriptions are ready
//!  - `std::thread::sleep` gives the worker's `subscription_reader` time to process
//!  - another sync channel signals the PUB when the SUB is done so the PUB
//!    keeps the connection alive until all messages have been received

use bytes::Bytes;
use compio::net::TcpListener;
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::publisher::PubSocket;
use monocoque_zmtp::subscriber::SubSocket;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Subscriptions set in `SocketOptions` are applied automatically at construction.
///
/// The SUB connects with `.with_subscribe(b"news.")` in its options; the PUB
/// sends one matching message followed by one non-matching message.  The SUB
/// must receive only the matching message without any explicit `.subscribe()` call.
#[test]
fn test_sub_options_subscriptions_are_applied() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    // SUB signals here after with_options returns (subscription bytes sent).
    let (sub_ready_tx, sub_ready_rx) = mpsc::channel::<()>();
    // SUB signals here after its recv loop finishes so the PUB can exit.
    let (client_done_tx, client_done_rx) = mpsc::channel::<()>();
    let (msg_tx, msg_rx) = mpsc::channel::<Option<Vec<Bytes>>>();

    let pub_handle = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut pub_sock = PubSocket::new();
                pub_sock.accept_subscriber(&listener).await.unwrap();

                // Wait for SUB to finish sending subscription bytes.
                sub_ready_rx.recv().unwrap();
                // Blocking sleep lets worker subscription_reader process the bytes.
                std::thread::sleep(Duration::from_millis(100));

                pub_sock
                    .send(vec![Bytes::from("news.breaking"), Bytes::from("story")])
                    .await
                    .unwrap();
                pub_sock
                    .send(vec![Bytes::from("weather.today")])
                    .await
                    .unwrap();
                pub_sock
                    .send(vec![Bytes::from("news.sports")])
                    .await
                    .unwrap();

                // Hold the connection open until the SUB has received its message.
                client_done_rx.recv().unwrap();
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();

                // Create SUB with subscription declared in options  -  no manual subscribe().
                let opts = SocketOptions::default().with_subscribe(Bytes::from("news."));
                let mut sub = SubSocket::with_options(stream, opts).await.unwrap();

                // Signal that subscription bytes have been sent.
                sub_ready_tx.send(()).unwrap();

                let first = compio::time::timeout(Duration::from_secs(5), sub.recv())
                    .await
                    .expect("recv timed out")
                    .unwrap();

                msg_tx.send(first).unwrap();
                client_done_tx.send(()).unwrap();
            });
    });

    pub_handle.join().expect("pub thread panicked");
    client.join().expect("client thread panicked");

    let received = msg_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("did not receive a message");

    let frames = received.expect("connection closed unexpectedly");
    assert!(
        frames[0].starts_with(b"news."),
        "expected a 'news.' message, got {:?}",
        frames[0]
    );
}

/// Multiple subscriptions set in `SocketOptions` are all applied.
///
/// Two topics are registered via options; only messages matching either prefix
/// should be delivered.
#[test]
fn test_sub_options_multiple_subscriptions() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (sub_ready_tx, sub_ready_rx) = mpsc::channel::<()>();
    let (client_done_tx, client_done_rx) = mpsc::channel::<()>();
    let (msgs_tx, msgs_rx) = mpsc::channel::<Vec<Vec<Bytes>>>();

    let pub_handle = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut pub_sock = PubSocket::new();
                pub_sock.accept_subscriber(&listener).await.unwrap();

                sub_ready_rx.recv().unwrap();
                std::thread::sleep(Duration::from_millis(100));

                for topic in [
                    "alerts.fire",
                    "ignore.me",
                    "metrics.cpu",
                    "ignore.also",
                    "alerts.critical",
                ] {
                    pub_sock.send(vec![Bytes::from(topic)]).await.unwrap();
                }

                // Keep connection alive until SUB finishes.
                client_done_rx.recv().unwrap();
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();

                let opts = SocketOptions::default()
                    .with_subscribe(Bytes::from("alerts."))
                    .with_subscribe(Bytes::from("metrics."));
                let mut sub = SubSocket::with_options(stream, opts).await.unwrap();

                sub_ready_tx.send(()).unwrap();

                let mut received = Vec::new();
                for _ in 0..3 {
                    match compio::time::timeout(Duration::from_secs(3), sub.recv()).await {
                        Ok(Ok(Some(frames))) => received.push(frames),
                        _ => break,
                    }
                }
                msgs_tx.send(received).unwrap();
                client_done_tx.send(()).unwrap();
            });
    });

    pub_handle.join().expect("pub thread panicked");
    client.join().expect("client thread panicked");

    let msgs = msgs_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        msgs.len(),
        3,
        "expected 3 matching messages, got {}",
        msgs.len()
    );

    for msg in &msgs {
        let topic = &msg[0];
        assert!(
            topic.starts_with(b"alerts.") || topic.starts_with(b"metrics."),
            "unexpected topic: {topic:?}"
        );
    }
}

/// Broadcast coalescing (Fix 4) delivers a rapid burst intact and in order.
///
/// The PUB pushes a tight burst of messages so they queue behind the worker,
/// which drains them into a single per-subscriber vectored write. Whatever the
/// actual batch sizes, the subscriber must receive every message exactly once,
/// in send order, byte-identical - coalescing must never reorder, merge, or drop
/// a frame.
#[test]
fn test_pub_broadcast_coalescing_burst() {
    const N: usize = 500;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (sub_ready_tx, sub_ready_rx) = mpsc::channel::<()>();
    let (client_done_tx, client_done_rx) = mpsc::channel::<()>();
    let (msgs_tx, msgs_rx) = mpsc::channel::<Vec<Vec<Bytes>>>();

    let pub_handle = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut pub_sock = PubSocket::new();
                pub_sock.accept_subscriber(&listener).await.unwrap();

                sub_ready_rx.recv().unwrap();
                std::thread::sleep(Duration::from_millis(100));

                // Tight burst: each message carries its sequence number as the
                // payload (a multipart [topic, seq] message).
                for i in 0..N {
                    pub_sock
                        .send(vec![
                            Bytes::from_static(b"burst"),
                            Bytes::from(i.to_string()),
                        ])
                        .await
                        .unwrap();
                }

                client_done_rx.recv().unwrap();
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();
                // Empty subscription = receive everything.
                let opts = SocketOptions::default().with_subscribe(Bytes::from_static(b""));
                let mut sub = SubSocket::with_options(stream, opts).await.unwrap();

                sub_ready_tx.send(()).unwrap();

                let mut received = Vec::with_capacity(N);
                for _ in 0..N {
                    match compio::time::timeout(Duration::from_secs(5), sub.recv()).await {
                        Ok(Ok(Some(frames))) => received.push(frames),
                        _ => break,
                    }
                }
                msgs_tx.send(received).unwrap();
                client_done_tx.send(()).unwrap();
            });
    });

    pub_handle.join().expect("pub thread panicked");
    client.join().expect("client thread panicked");

    let msgs = msgs_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_eq!(msgs.len(), N, "expected {N} messages, got {}", msgs.len());

    // Every message intact and in send order.
    for (i, msg) in msgs.iter().enumerate() {
        assert_eq!(msg.len(), 2, "message {i} should have 2 frames");
        assert_eq!(msg[0], Bytes::from_static(b"burst"));
        assert_eq!(msg[1], Bytes::from(i.to_string()), "out-of-order at {i}");
    }
}
