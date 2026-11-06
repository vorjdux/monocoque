//! libzmq PAIR against a monocoque PAIR.

use monocoque::zmq::{PairSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn test_interop_pair() {
    let (ready_tx, ready_rx) = mpsc::channel::<String>();
    let server = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut pair = PairSocket::with_options(
                    SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
                );
                ready_tx
                    .send(pair.bind("tcp://127.0.0.1:0").await.unwrap())
                    .unwrap();
                assert_eq!(pair.recv().await.unwrap(), [b"Ping"]);
                pair.send("Pong").await.unwrap();
                pair.close().await.unwrap();
            });
    });

    let endpoint = ready_rx.recv().unwrap();
    let ctx = zmq::Context::new();
    let sock = ctx.socket(zmq::PAIR).unwrap();
    sock.set_rcvtimeo(5000).unwrap();
    sock.connect(&endpoint).unwrap();
    sock.send("Ping", 0).unwrap();
    assert_eq!(sock.recv_string(0).unwrap().unwrap(), "Pong");
    server.join().unwrap();
}
