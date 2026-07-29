//! STREAM raw-TCP interop.
//!
//! A monocoque STREAM socket bridges plain TCP with no ZMTP handshake, so its
//! interop peer is an ordinary TCP client. This drives a real std TcpStream
//! against it and also exercises the multi-frame send fix: sending
//! `[routing_id, "", "wor", "ld"]` must write all payload frames in order
//! ("world"), not just the first.

use bytes::Bytes;
use monocoque::zmq::StreamSocket;
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

#[test]
fn interop_stream_raw_tcp_roundtrip() {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut server = StreamSocket::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(server.local_addr().unwrap()).unwrap();

                let routing_id = server.accept_raw().await.unwrap();

                // recv() surfaces connect/disconnect notifications ([id, "", ""])
                // as well as data. Skip empty-payload notifications and read
                // until the client's actual request bytes arrive.
                loop {
                    match server.recv().await {
                        Ok(Some(msg)) if msg.len() == 3 && !msg[2].is_empty() => {
                            if msg[2].as_ref() == b"hello" {
                                break;
                            }
                            let _ =
                                result_tx.send(Err(format!("STREAM recv wrong data: {:?}", msg[2])));
                            return;
                        }
                        Ok(Some(_)) => continue, // connect/disconnect notification
                        other => {
                            let _ = result_tx
                                .send(Err(format!("STREAM recv failed before data: {other:?}")));
                            return;
                        }
                    }
                }

                // Reply with several payload frames; all must reach the wire in
                // order, forming "world".
                server
                    .send(vec![
                        routing_id.clone(),
                        Bytes::new(),
                        Bytes::from_static(b"wor"),
                        Bytes::from_static(b"ld"),
                    ])
                    .await
                    .unwrap();

                let _ = result_tx.send(Ok(()));
                // Keep the peer alive briefly so the reply flushes before drop.
                monocoque::rt::sleep(Duration::from_millis(200)).await;
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut client = std::net::TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client.write_all(b"hello").unwrap();

    let mut reply = [0u8; 5];
    client
        .read_exact(&mut reply)
        .expect("raw TCP client did not receive the STREAM reply");
    assert_eq!(&reply, b"world", "STREAM send must write all payload frames in order");

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("STREAM server thread did not report")
        .expect("STREAM server side failed");
}
