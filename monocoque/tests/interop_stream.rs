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
