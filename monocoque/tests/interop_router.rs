//! libzmq DEALER against a monocoque ROUTER: identity routing both ways.

use bytes::Bytes;
use monocoque::zmq::{Msg, RouterSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn router_echo(reply: &'static str) -> (String, thread::JoinHandle<()>) {
    let (ready_tx, ready_rx) = mpsc::channel::<String>();
    let handle = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut router = RouterSocket::with_options(
                    SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
                );
                ready_tx
                    .send(router.bind("tcp://127.0.0.1:0").await.unwrap())
                    .unwrap();
                let msg = router.recv_owned().await.unwrap();
                let identity = msg.frame_bytes(0).unwrap();
                router
                    .send(Msg::from([identity, Bytes::from_static(reply.as_bytes())]))
                    .await
                    .unwrap();
                router.close().await.unwrap();
            });
    });
    (ready_rx.recv().unwrap(), handle)
}

#[test]
fn test_router_explicit_routing() {
    let (endpoint, server) = router_echo("World");
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"CLIENT_A").unwrap();
    dealer.set_rcvtimeo(5000).unwrap();
    dealer.connect(&endpoint).unwrap();
    dealer.send("Hello", 0).unwrap();
    assert_eq!(dealer.recv_string(0).unwrap().unwrap(), "World");
    server.join().unwrap();
}

#[test]
fn test_router_load_balancer_basic() {
    let (endpoint, server) = router_echo("Response from Router");
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"WORKER_1").unwrap();
    dealer.set_rcvtimeo(5000).unwrap();
    dealer.connect(&endpoint).unwrap();
    dealer.send("Task from worker", 0).unwrap();
    assert_eq!(
        dealer.recv_string(0).unwrap().unwrap(),
        "Response from Router"
    );
    server.join().unwrap();
}
