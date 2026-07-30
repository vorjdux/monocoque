//! libzmq CURVE interop.
//!
//! Proves the post-handshake CURVE MESSAGE cipher matches CurveZMQ (RFC 26): a
//! real libzmq peer decrypts monocoque's frames and monocoque decrypts libzmq's.
//! The prior build keyed messages with XChaCha20-Poly1305 over a SHA-256 of a
//! 3-way DH, so a libzmq peer completed the handshake and then failed to decrypt
//! the first MESSAGE; these tests are the regression guard for that.
//!
//! DEALER<->DEALER is used deliberately to isolate the cipher: it has neither the
//! REQ/REP empty-delimiter envelope nor ROUTER routing-id framing, so a failure
//! here is a CURVE failure and nothing else.

use bytes::Bytes;
use monocoque::zmq::{DealerSocket, SocketOptions};
use monocoque_zmtp::security::curve::CurveSecretKey;
use std::thread;
use std::time::Duration;

/// A deterministic CURVE keypair for the test. The secret is arbitrary bytes
/// (test material, not a secret); the public is derived through the real X25519
/// path so it matches what libzmq computes from the same secret.
fn keypair(seed: u8) -> (/* public */ [u8; 32], /* secret */ [u8; 32]) {
    let mut sec = [0u8; 32];
    for (i, b) in sec.iter_mut().enumerate() {
        *b = (i as u8)
            .wrapping_mul(31)
            .wrapping_add(seed)
            .wrapping_add(1);
    }
    let public = *CurveSecretKey::from_bytes(sec).public_key().as_bytes();
    (public, sec)
}

/// Some distro builds of libzmq are compiled without libsodium/CURVE. There is
/// nothing to interop against then, so skip rather than fail. CI installs a
/// CURVE-enabled libzmq, where these run for real.
fn curve_unavailable() -> bool {
    if zmq::has("curve") == Some(true) {
        return false;
    }
    eprintln!("skipping: local libzmq was built without CURVE support");
    true
}

/// monocoque is the CURVE server (binds/accepts); libzmq is the CURVE client.
#[test]
fn curve_interop_monocoque_server_libzmq_client() {
    if curve_unavailable() {
        return;
    }
    let (server_pub, server_sec) = keypair(7);
    let (client_pub, client_sec) = keypair(19);

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

                let opts = SocketOptions::default()
                    .with_curve_server(true)
                    .with_curve_keypair(server_pub, server_sec);
                let mut dealer = match DealerSocket::from_tcp_with_options(stream, opts).await {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = result_tx.send(Err(format!("server CURVE handshake failed: {e}")));
                        return;
                    }
                };

                // Decrypt a MESSAGE that libzmq sealed with CurveZMQ.
                match dealer.recv().await {
                    Ok(Some(msg)) if msg[0].as_ref() == b"ping-from-libzmq" => {}
                    other => {
                        let _ = result_tx.send(Err(format!(
                            "server could not decrypt libzmq MESSAGE: {other:?}"
                        )));
                        return;
                    }
                }

                // Seal a MESSAGE for libzmq to decrypt.
                dealer
                    .send(vec![Bytes::from_static(b"pong-from-monocoque")])
                    .await
                    .unwrap();
                let _ = result_tx.send(Ok(()));
            });
    });

    let addr = ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let sock = ctx.socket(zmq::DEALER).unwrap();
    sock.set_curve_serverkey(&server_pub).unwrap();
    sock.set_curve_publickey(&client_pub).unwrap();
    sock.set_curve_secretkey(&client_sec).unwrap();
    sock.set_rcvtimeo(5000).unwrap();
    sock.set_sndtimeo(5000).unwrap();
    sock.connect(&format!("tcp://{addr}")).unwrap();

    sock.send("ping-from-libzmq", 0).unwrap();
    let reply = sock
        .recv_bytes(0)
        .expect("libzmq client did not receive/decrypt monocoque's CURVE reply");
    assert_eq!(
        reply, b"pong-from-monocoque",
        "libzmq could not decrypt monocoque's CURVE MESSAGE"
    );

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque server thread did not report")
        .expect("monocoque server side failed");
}

/// libzmq is the CURVE server (binds); monocoque is the CURVE client (connects).
#[test]
fn curve_interop_libzmq_server_monocoque_client() {
    if curve_unavailable() {
        return;
    }
    let (server_pub, server_sec) = keypair(41);
    let (client_pub, client_sec) = keypair(83);

    let ctx = zmq::Context::new();
    let server = ctx.socket(zmq::DEALER).unwrap();
    server.set_curve_server(true).unwrap();
    server.set_curve_secretkey(&server_sec).unwrap();
    server.set_curve_publickey(&server_pub).unwrap();
    server.set_rcvtimeo(5000).unwrap();
    server.set_sndtimeo(5000).unwrap();
    server.bind("tcp://127.0.0.1:0").unwrap();
    let endpoint = server.get_last_endpoint().unwrap().unwrap();
    let addr = endpoint.trim_start_matches("tcp://").to_string();

    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let opts = SocketOptions::default()
                    .with_curve_serverkey(server_pub)
                    .with_curve_keypair(client_pub, client_sec);
                let mut dealer = match DealerSocket::connect_with_options(
                    &format!("tcp://{addr}"),
                    opts,
                )
                .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = result_tx.send(Err(format!("client CURVE handshake failed: {e}")));
                        return;
                    }
                };
                dealer
                    .send(vec![Bytes::from_static(b"ping-from-monocoque")])
                    .await
                    .unwrap();
                match dealer.recv().await {
                    Ok(Some(msg)) if msg[0].as_ref() == b"pong-from-libzmq" => {
                        let _ = result_tx.send(Ok(()));
                    }
                    other => {
                        let _ = result_tx.send(Err(format!(
                            "client could not decrypt libzmq MESSAGE: {other:?}"
                        )));
                    }
                }
            });
    });

    // libzmq server: decrypt monocoque's MESSAGE, reply with one it must decrypt.
    let msg = server
        .recv_bytes(0)
        .expect("libzmq server did not receive/decrypt monocoque's CURVE MESSAGE");
    assert_eq!(
        msg, b"ping-from-monocoque",
        "libzmq could not decrypt monocoque's CURVE MESSAGE"
    );
    server.send("pong-from-libzmq", 0).unwrap();

    result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("monocoque client thread did not report")
        .expect("monocoque client side failed");
}
