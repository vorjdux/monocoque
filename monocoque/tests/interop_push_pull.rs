//! monocoque PUSH/PULL against libzmq PUSH/PULL, both directions.

use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn test_monocoque_push_to_libzmq_pull() {
    let ctx = zmq::Context::new();
    let pull = ctx.socket(zmq::PULL).unwrap();
    pull.set_rcvtimeo(10_000).unwrap();
    pull.bind("tcp://127.0.0.1:*").unwrap();
    let endpoint = pull.get_last_endpoint().unwrap().unwrap();

    let push_thread = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut push = PushSocket::new();
                push.connect(&endpoint).await.unwrap();
                push.send("hello from monocoque").await.unwrap();
                push.close().await.unwrap();
            });
    });

    let msg = pull.recv_msg(0).expect("libzmq PULL got nothing");
    assert_eq!(msg.as_str(), Some("hello from monocoque"));
    push_thread.join().unwrap();
}

#[test]
fn test_libzmq_push_to_monocoque_pull() {
    let (ready_tx, ready_rx) = mpsc::channel::<String>();
    let server = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut pull = PullSocket::with_options(
                    SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
                );
                ready_tx
                    .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                    .unwrap();
                assert_eq!(pull.recv().await.unwrap(), [b"hello from libzmq"]);
            });
    });

    let endpoint = ready_rx.recv().unwrap();
    let ctx = zmq::Context::new();
    let push = ctx.socket(zmq::PUSH).unwrap();
    push.connect(&endpoint).unwrap();
    push.send("hello from libzmq", 0).unwrap();
    server.join().unwrap();
}
