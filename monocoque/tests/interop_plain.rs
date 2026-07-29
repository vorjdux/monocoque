//! libzmq PLAIN authentication interop.
//!
//! A monocoque PLAIN client authenticates to a real libzmq PLAIN server and
//! exchanges a request/reply. libzmq validates PLAIN credentials through ZAP, so
//! the test runs a real ZAP handler (a REP socket on inproc://zeromq.zap.01 in
//! the server's context) that approves the credentials, exactly as libzmq's own
//! authenticator would. This exercises the ZMTP PLAIN handshake on the wire.

use bytes::Bytes;
use monocoque::zmq::{ReqSocket, SocketOptions};
use std::thread;
use std::time::Duration;

#[test]
fn interop_monocoque_plain_client_libzmq_server() {
    let ctx = zmq::Context::new();

    // ZAP authenticator: bound before the PLAIN server accepts, on its own
    // thread (zmq sockets are not Send) using a clone of the shared context.
    let zap_ctx = ctx.clone();
    let (zap_ready_tx, zap_ready_rx) = std::sync::mpsc::channel::<()>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let zap = thread::spawn(move || {
        let sock = zap_ctx.socket(zmq::REP).unwrap();
        sock.bind("inproc://zeromq.zap.01").unwrap();
        sock.set_rcvtimeo(200).unwrap();
        zap_ready_tx.send(()).unwrap();
        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            let Ok(request) = sock.recv_multipart(0) else {
                continue; // rcv timeout; check the stop flag and retry
            };
            // ZAP request: [version, request_id, domain, address, identity,
            // mechanism, credentials...]. Approve with a 200 reply that echoes
            // the request id.
            let request_id = request.get(1).cloned().unwrap_or_default();
            let reply: Vec<&[u8]> = vec![b"1.0", &request_id, b"200", b"OK", b"user", b""];
            sock.send_multipart(reply, 0).unwrap();
        }
    });
    zap_ready_rx.recv().unwrap();

    let rep = ctx.socket(zmq::REP).unwrap();
    rep.set_plain_server(true).unwrap();
    rep.set_zap_domain("global").unwrap();
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
                let opts = SocketOptions::default().with_plain_credentials("alice", "secret");
                let mut req =
                    match ReqSocket::connect_with_options(&format!("tcp://{addr}"), opts).await {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = result_tx.send(Err(format!("PLAIN handshake failed: {e}")));
                            return;
                        }
                    };
                req.send(vec![Bytes::from_static(b"ping")]).await.unwrap();
                match req.recv().await {
                    Ok(Some(reply)) if reply.len() == 1 && reply[0].as_ref() == b"pong" => {
                        let _ = result_tx.send(Ok(()));
                    }
                    other => {
                        let _ = result_tx.send(Err(format!("unexpected reply: {other:?}")));
                    }
                }
            });
    });

    let request = rep
        .recv_bytes(0)
        .expect("libzmq PLAIN server got no request from monocoque");
    assert_eq!(request, b"ping");
    rep.send("pong", 0).unwrap();

    let outcome = result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque PLAIN client thread did not report");
    let _ = stop_tx.send(());
    let _ = zap.join();
    outcome.expect("monocoque PLAIN client side failed");
}
