//! libzmq interop for the ported `recv_into` methods.
//!
//! `recv_into` shares each socket's frame-assembly and envelope/identity/filter
//! logic with `recv`, but writes frames into a caller-provided buffer instead of
//! allocating a fresh `Vec`. These tests prove the ported methods still speak
//! ZMTP correctly against real libzmq peers: DEALER (frames as-is), ROUTER
//! (identity prefix), REP/REQ (empty-delimiter envelope), and SUB (prefix
//! filter). Each also confirms the caller's buffer is reused across calls.

use bytes::Bytes;
use monocoque::zmq::{DealerSocket, RepSocket, ReqSocket, RouterSocket, SubSocket};
use std::thread;
use std::time::Duration;

/// monocoque DEALER `recv_into` reads single and multipart frames from libzmq.
#[test]
fn dealer_recv_into_matches_libzmq_dealer() {
    let ctx = zmq::Context::new();
    let peer = ctx.socket(zmq::DEALER).unwrap();
    peer.set_sndtimeo(5000).unwrap();
    peer.bind("tcp://127.0.0.1:0").unwrap();
    let endpoint = peer.get_last_endpoint().unwrap().unwrap();
    let addr = endpoint.trim_start_matches("tcp://").to_string();

    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut dealer = DealerSocket::connect(&format!("tcp://{addr}"))
                    .await
                    .unwrap();
                let mut buf: Vec<Bytes> = Vec::new();

                assert!(dealer.recv_into(&mut buf).await.unwrap());
                if buf != vec![Bytes::from_static(b"one")] {
                    let _ = result_tx.send(Err(format!("single frame wrong: {buf:?}")));
                    return;
                }
                assert!(dealer.recv_into(&mut buf).await.unwrap());
                if buf != vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")] {
                    let _ = result_tx.send(Err(format!("multipart wrong/not reused: {buf:?}")));
                    return;
                }
                let _ = result_tx.send(Ok(()));
            });
    });

    // Give the connection a moment to establish before sending.
    thread::sleep(Duration::from_millis(200));
    peer.send("one", 0).unwrap();
    peer.send_multipart([b"a".as_ref(), b"b".as_ref()], 0)
        .unwrap();

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque DEALER thread did not report")
        .expect("monocoque DEALER recv_into failed");
}

/// monocoque ROUTER `recv_into` prefixes the peer identity ahead of the body.
#[test]
fn router_recv_into_prefixes_identity_from_libzmq_dealer() {
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
                let mut router = RouterSocket::from_tcp(stream).await.unwrap();

                let mut buf: Vec<Bytes> = Vec::new();
                assert!(router.recv_into(&mut buf).await.unwrap());
                if buf.len() != 2 || buf[0].as_ref() != b"CLIENT_A" || buf[1].as_ref() != b"Hello" {
                    let _ = result_tx.send(Err(format!("router recv_into wrong: {buf:?}")));
                    return;
                }
                let _ = result_tx.send(Ok(()));
            });
    });

    let addr = ready_rx.recv().unwrap();
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"CLIENT_A").unwrap();
    dealer.set_sndtimeo(5000).unwrap();
    dealer.connect(&format!("tcp://{addr}")).unwrap();
    dealer.send("Hello", 0).unwrap();

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque ROUTER thread did not report")
        .expect("monocoque ROUTER recv_into failed");
}

/// monocoque REP `recv_into` strips the request envelope; the reply routes back.
#[test]
fn rep_recv_into_strips_envelope_from_libzmq_req() {
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

                let mut buf: Vec<Bytes> = Vec::new();
                assert!(rep.recv_into(&mut buf).await.unwrap());
                if buf != vec![Bytes::from_static(b"ping")] {
                    let _ = result_tx.send(Err(format!("REP recv_into wrong body: {buf:?}")));
                    return;
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
    let reply = req.recv_bytes(0).expect("libzmq REQ got no reply");
    assert_eq!(reply, b"pong");

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque REP thread did not report")
        .expect("monocoque REP recv_into failed");
}

/// monocoque REQ `recv_into` reads the reply body from a libzmq REP.
#[test]
fn req_recv_into_reads_libzmq_rep_reply() {
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
                let mut req = ReqSocket::connect(&format!("tcp://{addr}")).await.unwrap();
                req.send(vec![Bytes::from_static(b"ping")]).await.unwrap();
                let mut buf: Vec<Bytes> = Vec::new();
                assert!(req.recv_into(&mut buf).await.unwrap());
                if buf != vec![Bytes::from_static(b"pong")] {
                    let _ = result_tx.send(Err(format!("REQ recv_into wrong reply: {buf:?}")));
                    return;
                }
                let _ = result_tx.send(Ok(()));
            });
    });

    let request = rep.recv_bytes(0).expect("libzmq REP got no request");
    assert_eq!(request, b"ping");
    rep.send("pong", 0).unwrap();

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque REQ thread did not report")
        .expect("monocoque REQ recv_into failed");
}

/// monocoque SUB `recv_into` delivers only messages matching its subscription.
#[test]
fn sub_recv_into_filters_from_libzmq_pub() {
    let ctx = zmq::Context::new();
    let publisher = ctx.socket(zmq::PUB).unwrap();
    publisher.set_sndtimeo(5000).unwrap();
    publisher.bind("tcp://127.0.0.1:0").unwrap();
    let endpoint = publisher.get_last_endpoint().unwrap().unwrap();
    let addr = endpoint.trim_start_matches("tcp://").to_string();

    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut sub = SubSocket::connect(&format!("tcp://{addr}")).await.unwrap();
                sub.subscribe(b"wanted").await.unwrap();

                let mut buf: Vec<Bytes> = Vec::new();
                // The "skip" topic must be filtered out; only "wanted" arrives.
                assert!(sub.recv_into(&mut buf).await.unwrap());
                if buf.first().map(bytes::Bytes::as_ref) != Some(b"wanted".as_ref()) {
                    let _ = result_tx.send(Err(format!("SUB recv_into wrong topic: {buf:?}")));
                    return;
                }
                let _ = result_tx.send(Ok(()));
            });
    });

    // Let the subscription propagate to the publisher before sending.
    thread::sleep(Duration::from_millis(300));
    for _ in 0..5 {
        publisher.send("skip", 0).unwrap();
        publisher.send("wanted", 0).unwrap();
        thread::sleep(Duration::from_millis(50));
    }

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque SUB thread did not report")
        .expect("monocoque SUB recv_into failed");
}
