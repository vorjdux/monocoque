//! Integration tests for the STREAM socket (raw TCP bridging).
//!
//! Tests use `std::net::TcpStream` (blocking) as the "plain TCP client"
//! so there is no ZMTP involvement on the client side at all.
//!
//! Each test runs in its own OS thread with a dedicated compio Runtime to
//! avoid residual-timer crosstalk from prior handshake timeouts.

use bytes::Bytes;
use monocoque_zmtp::stream::StreamSocket;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ─────────────────────────────────────────────────────────────────────────────
// Test: connection notification on accept
// ─────────────────────────────────────────────────────────────────────────────

/// When a raw TCP client connects, StreamSocket should receive a notification
/// message: [routing_id, empty, empty].
#[test]
fn test_stream_connection_notification() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (notif_tx, notif_rx) = mpsc::channel::<Vec<Bytes>>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut srv = StreamSocket::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(srv.local_addr().unwrap()).unwrap();

                // Accept one connection.
                srv.accept_raw().await.unwrap();

                // The reader task sends the connection notification synchronously
                // into the inbound queue; give it a moment.
                std::thread::sleep(Duration::from_millis(20));

                let msg = srv.recv().await.unwrap().expect("expected notification");
                notif_tx.send(msg).unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    // Connect with a plain TCP client  -  no ZMTP.
    let _client = std::net::TcpStream::connect(addr).unwrap();

    let msg = notif_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(msg.len(), 3, "notification must have 3 frames");
    assert_eq!(msg[0].len(), 8, "routing-id must be 8 bytes");
    assert!(msg[1].is_empty(), "frame 1 must be empty separator");
    assert!(msg[2].is_empty(), "notification data frame must be empty");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: raw data from plain TCP client → StreamSocket recv
// ─────────────────────────────────────────────────────────────────────────────

/// Data sent by a plain TCP client arrives as [routing_id, empty, data].
#[test]
fn test_stream_recv_raw_data() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (msg_tx, msg_rx) = mpsc::channel::<Vec<Bytes>>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut srv = StreamSocket::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(srv.local_addr().unwrap()).unwrap();

                srv.accept_raw().await.unwrap();

                // Drain notifications until we get a non-empty data frame.
                loop {
                    let msg = srv.recv().await.unwrap().expect("channel closed");
                    if !msg[2].is_empty() {
                        msg_tx.send(msg).unwrap();
                        break;
                    }
                }
            });
    });

    let addr = addr_rx.recv().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client.write_all(b"hello from plain TCP").unwrap();

    let msg = msg_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(msg.len(), 3);
    assert_eq!(msg[0].len(), 8, "routing-id must be 8 bytes");
    assert!(msg[1].is_empty(), "separator must be empty");
    assert_eq!(&msg[2][..], b"hello from plain TCP");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: StreamSocket send → data arrives at plain TCP client
// ─────────────────────────────────────────────────────────────────────────────

/// `send([routing_id, empty, data])` writes raw bytes to the identified peer.
#[test]
fn test_stream_send_raw_data() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut srv = StreamSocket::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(srv.local_addr().unwrap()).unwrap();

                let routing_id = srv.accept_raw().await.unwrap();

                // Wait for the connect notification.
                srv.recv().await.unwrap();

                // Send raw bytes to the client.
                srv.send(vec![
                    routing_id,
                    Bytes::new(),
                    Bytes::from("server says hello"),
                ])
                .await
                .unwrap();

                // Keep the runtime alive until the client closes the connection so
                // the background writer task has time to flush before we drop it.
                let _ = srv.recv().await;
            });
    });

    let addr = addr_rx.recv().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut buf = [0u8; 64];
    let n = client.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"server says hello");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: echo server  -  bidirectional raw TCP
// ─────────────────────────────────────────────────────────────────────────────

/// Full round-trip: client sends data, server echoes it back.
#[test]
fn test_stream_echo() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    // Echo server thread.
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut srv = StreamSocket::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(srv.local_addr().unwrap()).unwrap();

                srv.accept_raw().await.unwrap();

                // Echo up to 3 messages (notifications + data).
                for _ in 0..10 {
                    let msg = srv.recv().await.unwrap().expect("closed");
                    if msg[2].is_empty() {
                        continue; // skip notifications
                    }
                    // Echo data back to the same peer.
                    srv.send(msg).await.unwrap();
                    // Keep the runtime alive until the writer task flushes; the
                    // disconnect notification arrives when the client drops.
                    let _ = srv.recv().await;
                    return;
                }
            });
    });

    let addr = addr_rx.recv().unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    client.write_all(b"ping").unwrap();

    let mut buf = [0u8; 64];
    let n = client.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"ping");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: multiple peers  -  routing IDs are unique
// ─────────────────────────────────────────────────────────────────────────────

/// Three TCP clients connect; each gets a distinct routing ID.
/// The server should be able to send selectively to each client.
#[test]
fn test_stream_multi_peer_unique_routing_ids() {
    const N: usize = 3;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (ids_tx, ids_rx) = mpsc::channel::<Vec<Bytes>>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut srv = StreamSocket::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(srv.local_addr().unwrap()).unwrap();

                let mut routing_ids = Vec::new();
                for _ in 0..N {
                    let id = srv.accept_raw().await.unwrap();
                    routing_ids.push(id);
                }
                ids_tx.send(routing_ids).unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();

    let _clients: Vec<_> = (0..N)
        .map(|_| std::net::TcpStream::connect(addr).unwrap())
        .collect();

    let ids = ids_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(ids.len(), N, "expected {} routing IDs", N);

    // All routing IDs must be unique.
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), N, "routing IDs must be unique");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: disconnect removes peer from routing table
// ─────────────────────────────────────────────────────────────────────────────

/// After `disconnect(routing_id)`, the peer count drops and subsequent sends
/// to that routing ID are silently ignored.
#[test]
fn test_stream_disconnect_removes_peer() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (result_tx, result_rx) = mpsc::channel::<bool>();

    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut srv = StreamSocket::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(srv.local_addr().unwrap()).unwrap();

                let id = srv.accept_raw().await.unwrap();
                assert_eq!(srv.peer_count(), 1);

                srv.disconnect(&id);
                assert_eq!(srv.peer_count(), 0);

                // Sending to the disconnected peer should not error (silent drop).
                let res = srv
                    .send(vec![id, Bytes::new(), Bytes::from("ignored")])
                    .await;
                result_tx.send(res.is_ok()).unwrap();
            });
    });

    let addr = addr_rx.recv().unwrap();
    let _client = std::net::TcpStream::connect(addr).unwrap();

    let ok = result_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(ok, "send to disconnected peer should not error");
}
