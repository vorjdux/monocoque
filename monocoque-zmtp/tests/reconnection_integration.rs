//! Integration tests for automatic reconnection functionality.
//!
//! Design principles:
//! - Each OS thread has its own compio Runtime  -  avoids residual-timer crosstalk.
//! - The server holds the `TcpListener` open across connections so the client
//!   can reconnect without racing a new bind.
//! - The server sends messages *proactively* (no client request needed) so that
//!   `recv_with_reconnect` can collect a message on the freshly-reconnected stream.
//! - `std::thread::sleep` is used in base.rs for reconnect backoff to avoid the
//!   compio residual-timer issue; tests set `reconnect_ivl=10ms` for speed.

use bytes::Bytes;
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::dealer::DealerSocket;
use monocoque_zmtp::router::RouterSocket;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn fast_opts() -> SocketOptions {
    SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(10))
        .with_reconnect_ivl_max(Duration::from_millis(100))
        .with_max_reconnect_attempts(Some(20))
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: dealer detects server disconnect (EOF)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_dealer_detects_server_disconnect() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut router = RouterSocket::from_tcp(stream).await.unwrap();
                let _ = router.recv().await; // receive then drop
            });
    });

    let addr = addr_rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(20));

    compio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut dealer = DealerSocket::connect(addr).await.unwrap();
            dealer
                .send(vec![Bytes::new(), Bytes::from("ping")])
                .await
                .unwrap();

            let result = dealer.recv().await;
            match result {
                Ok(None) | Err(_) => {} // server closed the connection
                Ok(Some(_)) => panic!("expected connection closed"),
            }
        });
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: recv_with_reconnect reconnects after server restart
// ─────────────────────────────────────────────────────────────────────────────
//
// Server sequence (single thread, same listener):
//   1. accept → send "first" → drop (→ client gets EOF)
//   2. accept → send "second" → signal done
//
// Client sequence:
//   1. connect → recv() → "first"
//   2. recv_with_reconnect() → EOF → reconnect → "second"
//
// The listener stays open between steps so the client can re-connect.

#[test]
fn test_recv_with_reconnect_after_server_restart() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    // Server thread: sends proactively on both connections.
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                // First connection: send a message, then drop → client EOF.
                let (stream1, _) = listener.accept().await.unwrap();
                // Use DealerSocket on the server side so it can send without
                // needing a routing envelope (DealerSocket-to-DealerSocket works
                // fine at the ZMTP framing layer).
                let mut srv1 = DealerSocket::from_tcp(stream1).await.unwrap();
                srv1.send(vec![Bytes::from("first")]).await.unwrap();
                drop(srv1); // client sees EOF

                // Second connection (after client reconnects).
                let (stream2, _) = listener.accept().await.unwrap();
                let mut srv2 = DealerSocket::from_tcp(stream2).await.unwrap();
                srv2.send(vec![Bytes::from("second")]).await.unwrap();

                done_tx.send(()).unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Client thread.
    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut dealer = DealerSocket::connect_with_options(addr, fast_opts())
                    .await
                    .unwrap();

                // Receive the proactive "first" message from the first connection.
                let msg1 = compio::time::timeout(Duration::from_secs(5), dealer.recv())
                    .await
                    .expect("recv 1 timed out")
                    .expect("io error recv 1")
                    .expect("connection closed before first message");
                assert_eq!(msg1, vec![Bytes::from("first")], "wrong first message");

                // After the server drops the first connection, recv_with_reconnect
                // should detect EOF, reconnect, and return "second".
                let msg2 =
                    compio::time::timeout(Duration::from_secs(10), dealer.recv_with_reconnect())
                        .await
                        .expect("recv_with_reconnect timed out")
                        .expect("io error on reconnect recv")
                        .expect("connection closed without second message");

                assert_eq!(msg2, vec![Bytes::from("second")], "wrong second message");
            });
    });

    client.join().expect("client thread panicked");
    done_rx.recv_timeout(Duration::from_secs(15)).unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: send_with_reconnect reconnects after server restart
// ─────────────────────────────────────────────────────────────────────────────
//
// Coordination channels prevent the race where the server processes both
// accepts before the client ever connects:
//   drop_tx: server → client  ("first connection dropped, go reconnect")
//   reconnected_tx: client → server ("reconnected, now echo")

#[test]
fn test_send_with_reconnect_after_server_restart() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    // Server signals client that it has dropped the first connection.
    let (drop_tx, drop_rx) = mpsc::channel::<()>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                // First connection: receive one message, then drop.
                // Waiting for a client message (instead of dropping immediately)
                // ensures the client has fully connected before the drop happens.
                let (stream1, _) = listener.accept().await.unwrap();
                let mut router1 = RouterSocket::from_tcp(stream1).await.unwrap();
                let _ = router1.recv().await; // wait for client probe
                drop(router1); // → client's recv sees EOF
                drop_tx.send(()).unwrap(); // → tell client to call send_with_reconnect

                // Second connection: recv one message and echo.
                let (stream2, _) = listener.accept().await.unwrap();
                let mut router2 = RouterSocket::from_tcp(stream2).await.unwrap();
                let msg = router2.recv().await.unwrap().unwrap();
                router2.send(msg).await.unwrap();

                done_tx.send(()).unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    let client = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut dealer = DealerSocket::connect_with_options(addr, fast_opts())
                    .await
                    .unwrap();

                // Send a probe so the server knows we're connected and can drop.
                dealer
                    .send(vec![Bytes::new(), Bytes::from("probe")])
                    .await
                    .unwrap();

                // Wait for server to signal drop, then drain the EOF.
                drop_rx.recv().unwrap();
                let _ = dealer.recv().await; // Ok(None) or Err  -  both fine

                // send_with_reconnect reconnects and delivers the message.
                let payload = vec![Bytes::new(), Bytes::from("hello-reconnect")];
                dealer
                    .send_with_reconnect(payload.clone())
                    .await
                    .expect("send_with_reconnect error");

                // Router echoed the message; receive the reply.
                let reply = dealer
                    .recv()
                    .await
                    .expect("io error on reply recv")
                    .expect("connection closed before reply");

                assert_eq!(
                    reply.last().unwrap(),
                    &Bytes::from("hello-reconnect"),
                    "wrong reply payload"
                );
            });
    });

    client.join().expect("client thread panicked");
    done_rx.recv_timeout(Duration::from_secs(15)).unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: max_reconnect_attempts exhausted → NotConnected error
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_reconnect_max_attempts_exceeded() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    // Server: accept once, drop, listener also drops (no more accepts).
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let _router = RouterSocket::from_tcp(stream).await.unwrap();
                // Drop both  -  no more accepts available.
            });
    });

    let addr = addr_rx.recv().unwrap();

    let result = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let opts = SocketOptions::default()
                    .with_reconnect_ivl(Duration::from_millis(5))
                    .with_reconnect_ivl_max(Duration::from_millis(20))
                    .with_max_reconnect_attempts(Some(2));

                let mut dealer = DealerSocket::connect_with_options(addr, opts)
                    .await
                    .unwrap();

                // Force EOF.
                let _ = dealer.recv().await;

                // recv_with_reconnect must return NotConnected after 2 failed attempts.
                dealer.recv_with_reconnect().await
            })
    })
    .join()
    .expect("client thread panicked");

    assert!(result.is_err(), "expected error after exhausting attempts");
    // The error is NotConnected (attempts exhausted) or a connection error
    // (ConnectionRefused from the OS when no one is listening).
    let kind = result.unwrap_err().kind();
    assert!(
        matches!(
            kind,
            std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
        ),
        "unexpected error kind: {kind:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: from_tcp() sockets cannot reconnect (no stored endpoint)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_no_reconnect_without_stored_endpoint() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let _router = RouterSocket::from_tcp(stream).await.unwrap();
                // Drop immediately.
            });
    });

    let addr = addr_rx.recv().unwrap();

    let result = thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                // from_tcp() does NOT store the endpoint → cannot reconnect.
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();
                let mut dealer = DealerSocket::from_tcp(stream).await.unwrap();

                // Force EOF.
                let _ = dealer.recv().await;

                // recv_with_reconnect should fail immediately (no endpoint stored).
                dealer.recv_with_reconnect().await
            })
    })
    .join()
    .expect("client thread panicked");

    assert!(result.is_err(), "expected error without stored endpoint");
    let kind = result.unwrap_err().kind();
    assert!(
        matches!(
            kind,
            std::io::ErrorKind::Unsupported | std::io::ErrorKind::NotConnected
        ),
        "unexpected error kind: {kind:?}"
    );
}
