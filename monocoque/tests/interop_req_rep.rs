//! libzmq REQ/REP interop.
//!
//! Proves the ZMTP REQ/REP empty-delimiter envelope: monocoque REQ prepends the
//! delimiter and strips it from the reply; monocoque REP strips the request's
//! envelope and re-prepends it on the reply. Both directions are exercised.

use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

/// monocoque is REP (binds); libzmq is REQ (connects).
#[test]
fn interop_monocoque_rep_libzmq_req() {
    let (ready_tx, ready_rx) = mpsc::channel::<String>();
    let server = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut rep = RepSocket::with_options(opts());
                ready_tx
                    .send(rep.bind("tcp://127.0.0.1:0").await.unwrap())
                    .unwrap();
                let req = rep.recv().await.unwrap();
                assert_eq!(req, [b"ping"], "REP received the wrong request");
                rep.send("pong").await.unwrap();
                rep.close().await.unwrap();
            });
    });

    let endpoint = ready_rx.recv().unwrap();
    let ctx = zmq::Context::new();
    let req = ctx.socket(zmq::REQ).unwrap();
    req.set_rcvtimeo(5000).unwrap();
    req.connect(&endpoint).unwrap();
    req.send("ping", 0).unwrap();
    let reply = req
        .recv_bytes(0)
        .expect("libzmq REQ got no reply from monocoque REP");
    assert_eq!(reply, b"pong");
    server.join().unwrap();
}

/// libzmq is REP (binds); monocoque is REQ (connects).
#[test]
fn interop_libzmq_rep_monocoque_req() {
    let ctx = zmq::Context::new();
    let rep = ctx.socket(zmq::REP).unwrap();
    rep.set_rcvtimeo(5000).unwrap();
    rep.bind("tcp://127.0.0.1:*").unwrap();
    let endpoint = rep.get_last_endpoint().unwrap().unwrap();

    let client = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut req = ReqSocket::with_options(opts());
                req.connect(&endpoint).await.unwrap();
                req.send("ping").await.unwrap();
                assert_eq!(req.recv().await.unwrap(), [b"pong"]);
            });
    });

    let request = rep
        .recv_bytes(0)
        .expect("libzmq REP got no request from monocoque REQ");
    assert_eq!(request, b"ping");
    rep.send("pong", 0).unwrap();
    client.join().unwrap();
}
