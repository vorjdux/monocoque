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
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
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
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let msg = monocoque_core::rt::timeout(Duration::from_secs(5), pull.recv())
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

#[test]
fn test_push_pull_send_one_plain_and_coalesced() {
    use monocoque_core::options::SocketOptions;

    for coalesced in [false, true] {
        let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
        let (msg_tx, msg_rx) = mpsc::channel::<Vec<Bytes>>();

        let server = thread::spawn(move || {
            monocoque_core::rt::LocalRuntime::new()
                .unwrap()
                .block_on(async move {
                    let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                        .await
                        .unwrap();
                    addr_tx.send(listener.local_addr().unwrap()).unwrap();

                    let (stream, _) = listener.accept().await.unwrap();
                    let options = SocketOptions::default().with_write_coalescing(coalesced);
                    let mut push = PushSocket::from_tcp_with_options(stream, options)
                        .await
                        .unwrap();

                    push.send_one(Bytes::from_static(b"send-one"))
                        .await
                        .unwrap();
                    push.flush().await.unwrap();
                });
        });

        let addr = addr_rx.recv().unwrap();
        let client = thread::spawn(move || {
            monocoque_core::rt::LocalRuntime::new()
                .unwrap()
                .block_on(async move {
                    let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
                    let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                    let msg = monocoque_core::rt::timeout(Duration::from_secs(5), pull.recv())
                        .await
                        .expect("recv timed out")
                        .expect("io error")
                        .expect("connection closed");
                    msg_tx.send(msg).unwrap();
                });
        });

        client.join().expect("client thread panicked");
        assert_eq!(
            msg_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            vec![Bytes::from_static(b"send-one")]
        );
        server.join().expect("server thread panicked");
    }
}

/// PUSH buffers a coalesced message and then closes WITHOUT an explicit
/// `flush()`. With a non-zero LINGER, `close()` must drain the coalesced buffer so
/// the tail of the burst is not silently dropped. Regression for PUSH close
/// ignoring LINGER (it previously routed to `base.close()` which only shut the
/// stream down).
#[test]
fn test_push_close_flushes_coalesced_data_with_linger() {
    use monocoque_core::options::SocketOptions;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msg_tx, msg_rx) = mpsc::channel::<Vec<Bytes>>();

    let server = thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                // Coalescing on, non-zero linger so close() flushes.
                let options = SocketOptions::default()
                    .with_write_coalescing(true)
                    .with_linger(Some(Duration::from_secs(5)));
                let mut push = PushSocket::from_tcp_with_options(stream, options)
                    .await
                    .unwrap();

                // Buffer into the coalesce buffer, then close WITHOUT flush().
                push.send_one(Bytes::from_static(b"tail-of-burst"))
                    .await
                    .unwrap();
                push.close().await.unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();
    let client = thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let msg = monocoque_core::rt::timeout(Duration::from_secs(5), pull.recv())
                    .await
                    .expect("recv timed out")
                    .expect("io error")
                    .expect("connection closed before coalesced data was flushed");
                msg_tx.send(msg).unwrap();
            });
    });

    client.join().expect("client thread panicked");
    assert_eq!(
        msg_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        vec![Bytes::from_static(b"tail-of-burst")],
        "close() must flush coalesced data when LINGER is non-zero"
    );
    server.join().expect("server thread panicked");
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
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut push = PushSocket::from_tcp(stream).await.unwrap();

                for i in 0..N {
                    push.send(vec![Bytes::from(format!("msg-{i}"))])
                        .await
                        .unwrap();
                }
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Client thread: PULL connects and receives all N messages.
    let client = thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let mut received = Vec::new();
                for _ in 0..N {
                    let msg = monocoque_core::rt::timeout(Duration::from_secs(5), pull.recv())
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
    assert_eq!(received.len(), N, "expected {N} messages");
    for (i, msg) in received.iter().enumerate() {
        assert_eq!(
            msg,
            &vec![Bytes::from(format!("msg-{i}"))],
            "message {i} mismatch"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: reversed topology (PULL binds, PUSH connects)
// ─────────────────────────────────────────────────────────────────────────────

/// PULL binds and PUSH connects  -  confirms messages flow in this topology too.
#[test]
fn test_pull_bind_push_connect() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msg_tx, msg_rx) = mpsc::channel::<Vec<Bytes>>();

    // Server thread: PULL binds and waits for a message.
    thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let msg = monocoque_core::rt::timeout(Duration::from_secs(5), pull.recv())
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
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
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

// ─────────────────────────────────────────────────────────────────────────────
// Test: vectored write path (Fix 1) delivers large frames intact
// ─────────────────────────────────────────────────────────────────────────────

/// With a low `vectored_write_threshold`, PUSH sends both a large single-frame
/// message (long-form length header) and a multipart message that mixes a small
/// frame with a large one. Exercising the vectored path (header + body as an
/// iovec, no body copy) must produce byte-identical messages on the PULL side.
#[test]
fn test_push_pull_vectored_large_frame() {
    use monocoque_core::options::SocketOptions;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msg_tx, msg_rx) = mpsc::channel::<Vec<Vec<Bytes>>>();

    // 64 KiB body forces the long-form (9-byte) frame header.
    let big = Bytes::from(vec![0xABu8; 64 * 1024]);
    let small = Bytes::from_static(b"topic");

    let big_srv = big.clone();
    let small_srv = small.clone();

    // Server thread: PUSH binds with a 256-byte vectored threshold and sends.
    thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let opts = SocketOptions::default().with_vectored_write_threshold(256);
                let mut push = PushSocket::from_tcp_with_options(stream, opts)
                    .await
                    .unwrap();

                // Single large frame (vectored path).
                push.send(vec![big_srv.clone()]).await.unwrap();
                // Multipart: small frame + large frame (still vectored since one
                // frame exceeds the threshold).
                push.send(vec![small_srv.clone(), big_srv.clone()])
                    .await
                    .unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Client thread: PULL connects and receives both messages.
    let client = thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let mut received = Vec::new();
                for _ in 0..2 {
                    let msg = monocoque_core::rt::timeout(Duration::from_secs(5), pull.recv())
                        .await
                        .expect("recv timed out")
                        .expect("io error")
                        .expect("connection closed");
                    received.push(msg);
                }
                msg_tx.send(received).unwrap();
            });
    });

    client.join().expect("client thread panicked");

    let received = msg_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0], vec![big.clone()]);
    assert_eq!(received[1], vec![small, big]);
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: recv_batch (Fix 2) drains a burst with a single await, in order
// ─────────────────────────────────────────────────────────────────────────────

/// PUSH sends a batch of small messages in one kernel write; PULL drains them
/// with `recv_batch`. Every message must come back exactly once, in send order.
#[test]
fn test_push_pull_recv_batch() {
    const N: usize = 1000;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msg_tx, msg_rx) = mpsc::channel::<Vec<Vec<Bytes>>>();

    // Server thread: PUSH binds and sends N small messages via send_batch.
    thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = monocoque_core::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut push = PushSocket::from_tcp(stream).await.unwrap();

                let batch: Vec<Vec<Bytes>> =
                    (0..N).map(|i| vec![Bytes::from(i.to_string())]).collect();
                push.send_batch(batch).await.unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Client thread: PULL drains messages with recv_batch until it has all N.
    let client = thread::spawn(move || {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                let mut received = Vec::with_capacity(N);
                while received.len() < N {
                    match monocoque_core::rt::timeout(Duration::from_secs(5), pull.recv_batch())
                        .await
                    {
                        Ok(Ok(Some(batch))) => received.extend(batch),
                        _ => break,
                    }
                }
                msg_tx.send(received).unwrap();
            });
    });

    client.join().expect("client thread panicked");

    let received = msg_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(received.len(), N, "expected {N} messages");
    for (i, msg) in received.iter().enumerate() {
        assert_eq!(
            msg,
            &vec![Bytes::from(i.to_string())],
            "out-of-order at {i}"
        );
    }
}
