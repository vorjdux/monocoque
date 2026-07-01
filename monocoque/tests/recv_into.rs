//! `PullSocket::recv_into` correctness.
//!
//! `recv_into` writes a message's frames into a caller-provided buffer instead of
//! allocating a fresh `Vec` per message. These tests check it reads the same
//! frames as `recv` (single and multipart), clears the buffer on entry, and
//! reports a closed connection.

use bytes::Bytes;
use monocoque::SocketOptions;
use monocoque::rt::TcpListener;
use monocoque::zmq::{PullSocket, PushSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn test_recv_into_reads_single_and_multipart_then_eof() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    // PUSH side: bind, announce the port, send a few messages, then close.
    let sender = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut push = PushSocket::from_tcp(stream).await.unwrap();

                push.send(vec![Bytes::from("alpha")]).await.unwrap();
                push.send(vec![Bytes::from("beta")]).await.unwrap();
                push.send(vec![
                    Bytes::from("p1"),
                    Bytes::from("p2"),
                    Bytes::from("p3"),
                ])
                .await
                .unwrap();
                // Dropping `push` closes the connection so the reader sees EOF.
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let count = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut pull = PullSocket::connect(addr).await.unwrap();
                let mut buf: Vec<Bytes> = Vec::new();

                // Same reused buffer across every call.
                assert!(pull.recv_into(&mut buf).await.unwrap());
                assert_eq!(buf, vec![Bytes::from("alpha")]);

                assert!(pull.recv_into(&mut buf).await.unwrap());
                assert_eq!(
                    buf,
                    vec![Bytes::from("beta")],
                    "buffer not refilled cleanly"
                );

                assert!(pull.recv_into(&mut buf).await.unwrap());
                assert_eq!(
                    buf,
                    vec![Bytes::from("p1"), Bytes::from("p2"), Bytes::from("p3")],
                    "multipart frames not preserved"
                );

                // Sender has closed: recv_into reports the connection is done and
                // leaves the buffer cleared.
                let got = recv_into_until_closed(&mut pull, &mut buf).await;
                assert!(!got, "expected EOF after the sender closed");
                assert!(buf.is_empty(), "buffer should be cleared on entry");
                3usize
            })
    })
    .join()
    .expect("reader thread panicked");

    assert_eq!(count, 3);
    sender.join().expect("sender thread panicked");
}

/// `recv_into` for the first message, then `try_recv_into` to drain the rest of a
/// coalesced burst, must reproduce every message in order using one reused buffer.
#[test]
fn test_try_recv_into_drains_a_coalesced_burst() {
    const N: usize = 5;
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    let sender = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                // Coalesce so the burst arrives together and the reader can drain it
                // with try_recv_into after a single recv_into.
                let mut push = PushSocket::from_tcp_with_options(
                    stream,
                    SocketOptions::default().with_write_coalescing(true),
                )
                .await
                .unwrap();
                for i in 0..N {
                    push.send(vec![Bytes::from(format!("m{i}"))]).await.unwrap();
                }
                push.flush().await.unwrap();
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let got = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut pull = PullSocket::connect(addr).await.unwrap();
                let mut buf: Vec<Bytes> = Vec::new();
                let mut got: Vec<String> = Vec::new();

                while got.len() < N {
                    if !pull.recv_into(&mut buf).await.unwrap() {
                        break;
                    }
                    got.push(String::from_utf8(buf[0].to_vec()).unwrap());
                    // Drain everything else already buffered from the same read.
                    while pull.try_recv_into(&mut buf).unwrap() {
                        got.push(String::from_utf8(buf[0].to_vec()).unwrap());
                    }
                }
                got
            })
    })
    .join()
    .expect("reader thread panicked");

    let expected: Vec<String> = (0..N).map(|i| format!("m{i}")).collect();
    assert_eq!(got, expected, "burst not drained in order");
    sender.join().expect("sender thread panicked");
}

/// Receive until the connection closes (returns `false`), draining any late
/// message that races ahead of the connection-close first. Panics on error or
/// timeout.
async fn recv_into_until_closed(pull: &mut PullSocket, buf: &mut Vec<Bytes>) -> bool {
    loop {
        let got = monocoque::rt::timeout(Duration::from_secs(5), pull.recv_into(buf))
            .await
            .unwrap_or_else(|_| panic!("recv_into timed out waiting for EOF"))
            .expect("recv_into error");
        if !got {
            return false;
        }
        // A late message arrived before the close; keep draining.
    }
}
