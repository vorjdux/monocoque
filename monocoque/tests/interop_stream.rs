//! STREAM raw-TCP interop.
//!
//! A STREAM socket bridges plain TCP with no ZMTP handshake, so its interop
//! peer is an ordinary TCP client. Checks libzmq's framing: a connect
//! notification, `[identity, data]` on receive, every data frame of a send
//! written in order, and `[identity, ""]` closing the connection.

use bytes::Bytes;
use monocoque::zmq::{Msg, SocketOptions, StreamSocket};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn interop_stream_raw_tcp_roundtrip() {
    let (ready_tx, ready_rx) = mpsc::channel::<String>();
    let server = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut server = StreamSocket::with_options(
                    SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
                );
                ready_tx
                    .send(server.bind("tcp://127.0.0.1:0").await.unwrap())
                    .unwrap();

                let hello = server.recv_owned().await.unwrap();
                assert_eq!(hello.len(), 2);
                assert!(
                    hello.get(1).unwrap().is_empty(),
                    "first comes the connect notification"
                );
                let identity = hello.frame_bytes(0).unwrap();

                let data = server.recv_owned().await.unwrap();
                assert_eq!(data.get(0), Some(&identity[..]));
                assert_eq!(data.get(1), Some(&b"hello"[..]));

                server
                    .send(Msg::from([
                        identity.clone(),
                        Bytes::from_static(b"wor"),
                        Bytes::from_static(b"ld"),
                    ]))
                    .await
                    .unwrap();
                server
                    .send(Msg::from([identity.clone(), Bytes::new()]))
                    .await
                    .unwrap();
                let bye = server.recv_owned().await.unwrap();
                assert_eq!(
                    bye,
                    [&identity[..], b""],
                    "then the disconnect notification"
                );
            });
    });

    let endpoint = ready_rx.recv().unwrap();
    let mut client = std::net::TcpStream::connect(endpoint.trim_start_matches("tcp://")).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client.write_all(b"hello").unwrap();
    let mut reply = Vec::new();
    client.read_to_end(&mut reply).unwrap();
    assert_eq!(reply, b"world", "all data frames in order, then close");
    server.join().unwrap();
}

/// A STREAM socket offers what the ZMTP sockets do for looking at itself:
/// the monitor, its options, its endpoints and the connection a message came
/// from.
#[test]
fn stream_socket_can_be_inspected_like_the_other_sockets() {
    use monocoque::zmq::SocketEvent;

    monocoque::rt::LocalRuntime::new().unwrap().block_on(async {
        let mut server = StreamSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        let events = server.monitor();
        let endpoint = server.bind("tcp://127.0.0.1:0").await.unwrap();
        assert_eq!(server.last_endpoint().as_deref(), Some(endpoint.as_str()));
        assert!(!server.has_message());
        assert!(server.peer_ids().is_empty());

        let mut client =
            std::net::TcpStream::connect(endpoint.trim_start_matches("tcp://")).unwrap();
        client.write_all(b"hi").unwrap();

        let hello = server.recv_owned().await.unwrap();
        assert!(hello.get(1).unwrap().is_empty(), "the connect notification");
        assert_eq!(server.peer_ids().len(), 1);
        assert_eq!(
            server.peer_property("Peer-Address").as_deref(),
            Some(&b"127.0.0.1"[..])
        );
        assert!(server.last_metadata().is_some());
        assert!(server.last_peer().is_some());
        assert_eq!(server.recv_owned().await.unwrap().get(1), Some(&b"hi"[..]));
        assert!(server.can_send());

        server.update_options(|o| o.with_send_timeout(Duration::from_secs(1)));
        assert_eq!(server.options().send_timeout, Some(Duration::from_secs(1)));
        server.flush().await.unwrap();

        assert!(
            events
                .try_iter()
                .any(|e| matches!(e, SocketEvent::Accepted(_))),
            "the monitor reports the accepted connection"
        );
        server.unbind(&endpoint).unwrap();
        assert!(server.unbind(&endpoint).is_err(), "it is no longer bound");
        assert_eq!(server.peer_count(), 1, "an accepted connection stays");
    });
}
