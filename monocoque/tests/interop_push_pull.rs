//! Interop test: monocoque PUSH/PULL ↔ libzmq PUSH/PULL
//!
//! Uses the `zmq` crate (FFI bindings to libzmq) on one side and
//! monocoque on the other, verifying wire-level compatibility.

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque::zmq::{PullSocket, PushSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// ── monocoque PUSH → libzmq PULL ─────────────────────────────────────────────

#[test]
fn test_monocoque_push_to_libzmq_pull() {
    let (addr_tx, addr_rx) = mpsc::channel::<String>();
    let (result_tx, result_rx) = mpsc::channel::<Result<(), String>>();
    let (push_err_tx, push_err_rx) = mpsc::channel::<String>();

    // libzmq PULL server  -  generous timeout so it outlasts our push thread
    thread::spawn(move || {
        let ctx = zmq::Context::new();
        let pull = ctx.socket(zmq::PULL).unwrap();
        pull.bind("tcp://127.0.0.1:*").unwrap();
        let endpoint = pull.get_last_endpoint().unwrap().unwrap();
        addr_tx.send(endpoint).unwrap();

        pull.set_rcvtimeo(10_000).unwrap();
        match pull.recv_msg(0) {
            Ok(msg) => {
                if msg.as_str() == Some("hello from monocoque") {
                    result_tx.send(Ok(())).unwrap();
                } else {
                    result_tx
                        .send(Err(format!("Unexpected message: {:?}", &*msg)))
                        .unwrap();
                }
            }
            Err(e) => result_tx.send(Err(format!("recv error: {e}"))).unwrap(),
        }
    });

    let endpoint = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let addr: std::net::SocketAddr = endpoint.strip_prefix("tcp://").unwrap().parse().unwrap();

    // monocoque PUSH client  -  join to capture panics/errors
    let push_thread = thread::spawn(move || {
        let t0 = Instant::now();
        let r = compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut push = PushSocket::<TcpStream>::connect(addr).await?;
                push.send(vec![Bytes::from("hello from monocoque")]).await?;
                Ok::<(), std::io::Error>(())
            });
        eprintln!(
            "[PUSH] block_on took {:?}, result={:?}",
            t0.elapsed(),
            r.as_ref().err()
        );
        if let Err(e) = r {
            push_err_tx.send(e.to_string()).ok();
        }
    });

    push_thread.join().expect("monocoque PUSH thread panicked");

    if let Ok(err) = push_err_rx.try_recv() {
        panic!("monocoque PUSH error: {err}");
    }

    let result = result_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(result.is_ok(), "monocoque→libzmq push failed: {result:?}");
}

// ── libzmq PUSH → monocoque PULL ─────────────────────────────────────────────

#[test]
fn test_libzmq_push_to_monocoque_pull() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (result_tx, result_rx) = mpsc::channel::<Result<(), String>>();

    // monocoque PULL server
    thread::spawn(move || {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                addr_tx.send(addr).unwrap();

                let (stream, _) = listener.accept().await.unwrap();
                let mut pull = PullSocket::from_tcp(stream).await.unwrap();

                match pull.recv().await {
                    Ok(Some(msg)) if msg[0] == b"hello from libzmq"[..] => {
                        result_tx.send(Ok(())).unwrap();
                    }
                    Ok(Some(msg)) => result_tx
                        .send(Err(format!("Unexpected message: {msg:?}")))
                        .unwrap(),
                    Ok(None) => result_tx.send(Err("connection closed".into())).unwrap(),
                    Err(e) => result_tx.send(Err(e.to_string())).unwrap(),
                }
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    // libzmq PUSH client
    thread::spawn(move || {
        let ctx = zmq::Context::new();
        let push = ctx.socket(zmq::PUSH).unwrap();
        push.connect(&format!("tcp://{addr}")).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        push.send("hello from libzmq", 0).unwrap();
    });

    let result = result_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(result.is_ok(), "libzmq→monocoque push failed: {result:?}");
}
