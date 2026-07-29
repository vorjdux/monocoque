//! libzmq REQ/REP interop.
//!
//! Proves the ZMTP REQ/REP empty-delimiter envelope: monocoque REQ prepends the
//! delimiter and strips it from the reply; monocoque REP strips the request's
//! envelope and re-prepends it on the reply. Without this a real libzmq REQ/REP
//! peer cannot exchange messages. Both directions are exercised.

use bytes::Bytes;
use monocoque::zmq::{RepSocket, ReqSocket};
use std::thread;
use std::time::Duration;

/// monocoque is REP (binds/accepts); libzmq is REQ (connects).
#[test]
fn interop_monocoque_rep_libzmq_req() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = monocoque::rt::TcpListener::bind("127.0.0.1:0")
                    .await
                    .unwrap();
                ready_tx.send(listener.local_addr().unwrap()).unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let mut rep = RepSocket::from_tcp(stream).await.unwrap();

                match rep.recv().await {
                    Ok(Some(req)) if req.len() == 1 && req[0].as_ref() == b"ping" => {}
                    other => {
                        let _ = result_tx.send(Err(format!("REP recv wrong request: {other:?}")));
                        return;
                    }
                }
                rep.send(vec![Bytes::from_static(b"pong")]).await.unwrap();
                let _ = result_tx.send(Ok(()));
            });
    });

    let addr = ready_rx.recv().unwrap();
    let ctx = zmq::Context::new();
    let req = ctx.socket(zmq::REQ).unwrap();
    req.set_rcvtimeo(5000).unwrap();
    req.set_sndtimeo(5000).unwrap();
    req.connect(&format!("tcp://{addr}")).unwrap();

    req.send("ping", 0).unwrap();
    let reply = req.recv_bytes(0).expect("libzmq REQ got no reply from monocoque REP");
    assert_eq!(reply, b"pong", "libzmq REQ could not exchange with monocoque REP");

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque REP thread did not report")
        .expect("monocoque REP side failed");
}

/// libzmq is REP (binds); monocoque is REQ (connects).
#[test]
fn interop_libzmq_rep_monocoque_req() {
    let ctx = zmq::Context::new();
    let rep = ctx.socket(zmq::REP).unwrap();
    rep.set_rcvtimeo(5000).unwrap();
    rep.set_sndtimeo(5000).unwrap();
    rep.bind("tcp://127.0.0.1:0").unwrap();
    let endpoint = rep.get_last_endpoint().unwrap().unwrap();
    let addr = endpoint.trim_start_matches("tcp://").to_string();

    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut req = match ReqSocket::connect(&format!("tcp://{addr}")).await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = result_tx.send(Err(format!("REQ connect failed: {e}")));
                        return;
                    }
                };
                req.send(vec![Bytes::from_static(b"ping")]).await.unwrap();
                match req.recv().await {
                    Ok(Some(reply)) if reply.len() == 1 && reply[0].as_ref() == b"pong" => {
                        let _ = result_tx.send(Ok(()));
                    }
                    other => {
                        let _ = result_tx.send(Err(format!("REQ recv wrong reply: {other:?}")));
                    }
                }
            });
    });

    let request = rep.recv_bytes(0).expect("libzmq REP got no request from monocoque REQ");
    assert_eq!(request, b"ping", "libzmq REP could not read monocoque REQ request");
    rep.send("pong", 0).unwrap();

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque REQ thread did not report")
        .expect("monocoque REQ side failed");
}
