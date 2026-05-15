//! Integration tests for the PUSH/PULL pipeline pattern.
//!
//! Each test spawns its own compio Runtime in a dedicated OS thread to avoid
//! residual-timer crosstalk from prior handshake timeouts.
//!
//! Coordination between threads uses `std::sync::mpsc` channels.

use bytes::Bytes;
use monocoque_zmtp::pull::PullSocket;
use monocoque_zmtp::push::PushSocket;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ─────────────────────────────────────────────────────────────────────────────
// Test: basic PUSH bind / PULL connect
// ─────────────────────────────────────────────────────────────────────────────

/// PUSH binds on an OS-assigned port, PULL connects, PUSH sends one message,
/// PULL receives it.
#[test]
fn test_push_pull_basic() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msg_tx, msg_rx) = mpsc::channel::<Vec<Bytes>>();

    // Server thread: PUSH binds and sends one message.
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut push = PushSocket::from_tcp(stream).await.unwrap();

                push.send(vec![Bytes::from("hello pipeline")])
                    .await
                    .unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Client thread: PULL connects and receives the message.
    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let msg = compio::time::timeout(Duration::from_secs(5), pull.recv())
                    .await
                    .expect("recv timed out")
                    .expect("io error")
                    .expect("connection closed");
                msg_tx.send(msg).unwrap();
            });
    });

    client.join().expect("client thread panicked");

    let msg = msg_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(msg, vec![Bytes::from("hello pipeline")]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: multiple messages through the pipeline
// ─────────────────────────────────────────────────────────────────────────────

/// Sends 5 messages through the pipeline and receives all 5 in order.
#[test]
fn test_push_pull_multi_message() {
    const N: usize = 5;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msgs_tx, msgs_rx) = mpsc::channel::<Vec<Vec<Bytes>>>();

    // Server thread: PUSH binds and sends N messages.
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut push = PushSocket::from_tcp(stream).await.unwrap();

                for i in 0..N {
                    push.send(vec![Bytes::from(format!("msg-{}", i))])
                        .await
                        .unwrap();
                }
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Client thread: PULL connects and receives all N messages.
    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let mut received = Vec::new();
                for _ in 0..N {
                    let msg = compio::time::timeout(Duration::from_secs(5), pull.recv())
                        .await
                        .expect("recv timed out")
                        .expect("io error")
                        .expect("connection closed");
                    received.push(msg);
                }
                msgs_tx.send(received).unwrap();
            });
    });

    client.join().expect("client thread panicked");

    let received = msgs_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(received.len(), N, "expected {} messages", N);
    for (i, msg) in received.iter().enumerate() {
        assert_eq!(
            msg,
            &vec![Bytes::from(format!("msg-{}", i))],
            "message {} mismatch",
            i
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: reversed topology (PULL binds, PUSH connects)
// ─────────────────────────────────────────────────────────────────────────────

/// PULL binds and PUSH connects — confirms messages flow in this topology too.
#[test]
fn test_pull_bind_push_connect() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msg_tx, msg_rx) = mpsc::channel::<Vec<Bytes>>();

    // Server thread: PULL binds and waits for a message.
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let msg = compio::time::timeout(Duration::from_secs(5), pull.recv())
                    .await
                    .expect("recv timed out")
                    .expect("io error")
                    .expect("connection closed");
                msg_tx.send(msg).unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Client thread: PUSH connects and sends one message.
    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();
                let mut push = PushSocket::from_tcp(stream).await.unwrap();

                push.send(vec![Bytes::from("reversed topology")])
                    .await
                    .unwrap();
            });
    });

    client.join().expect("client thread panicked");

    let msg = msg_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(msg, vec![Bytes::from("reversed topology")]);
}
