//! The ZAP request a server sends must carry the connecting peer's real address.
//!
//! RFC 27 puts the peer address in the `Address` frame so a handler can apply IP
//! allow/deny policy. A server that sends a placeholder instead silently disables
//! that policy. These tests run the full ZMTP handshake over TCP against a ZAP
//! handler that records every address it is asked about.
//!
//! Each test binary is its own process and the ZAP endpoint is a process-global
//! inproc name, so this file holds the only ZAP server bound in its process.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::security::curve::CurveSecretKey;
use monocoque::zmq::security::zap::{ZapRequest, ZapResponse};
use monocoque::zmq::security::zap_handler::{ZapHandler, spawn_zap_server};
use monocoque::zmq::{DealerSocket, SocketOptions};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct RecordingHandler {
    addresses: Mutex<Vec<String>>,
}

impl ZapHandler for RecordingHandler {
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        self.addresses.lock().unwrap().push(request.address.clone());
        ZapResponse::success(request.request_id.clone(), "peer")
    }
}

/// A deterministic CURVE keypair (public, secret) derived from `seed`.
fn keypair(seed: u8) -> ([u8; 32], [u8; 32]) {
    let secret = [seed; 32];
    let public = *CurveSecretKey::from_bytes(secret).public_key().as_bytes();
    (public, secret)
}

/// Accept one connection with `server_opts`, connect with `client_opts`, and
/// return once both handshakes have completed.
async fn handshake_pair(server_opts: SocketOptions, client_opts: SocketOptions) {
    handshake_pair_on("tcp://127.0.0.1:0", server_opts, client_opts).await;
}

/// The same over the endpoint `bind_to`.
async fn handshake_pair_on(bind_to: &str, server_opts: SocketOptions, client_opts: SocketOptions) {
    let mut server = DealerSocket::with_options(server_opts);
    let ep = server.bind(bind_to).await.unwrap();
    let mut client = DealerSocket::with_options(client_opts);
    client.connect(&ep).await.unwrap();
    // The server only learns of a peer once its handshake, ZAP included, is done.
    while server.peer_count() == 0 {
        monocoque::rt::sleep(Duration::from_millis(1)).await;
    }
}

#[test]
fn zap_requests_carry_the_tcp_peer_ip() {
    LocalRuntime::new().unwrap().block_on(async {
        let handler = Arc::new(RecordingHandler::default());
        spawn_zap_server(Arc::clone(&handler)).unwrap();
        let timeout = Duration::from_secs(5);

        // PLAIN server with ZAP.
        handshake_pair(
            SocketOptions::default()
                .with_plain_server(true)
                .with_zap_domain("test")
                .with_handshake_timeout(timeout),
            SocketOptions::default()
                .with_plain_credentials("alice", "secret")
                .with_handshake_timeout(timeout),
        )
        .await;

        // CURVE server with ZAP.
        let (server_public, server_secret) = keypair(1);
        let (client_public, client_secret) = keypair(2);
        handshake_pair(
            SocketOptions::default()
                .with_curve_server(true)
                .with_curve_keypair(server_public, server_secret)
                .with_zap_domain("test")
                .with_handshake_timeout(timeout),
            SocketOptions::default()
                .with_curve_keypair(client_public, client_secret)
                .with_curve_serverkey(server_public)
                .with_handshake_timeout(timeout),
        )
        .await;

        let seen = handler.addresses.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec!["127.0.0.1".to_string(), "127.0.0.1".to_string()],
            "each ZAP request (PLAIN, then CURVE) must name the TCP peer's IP"
        );

        // A Unix socket peer has no address. The request carries an empty
        // one and the handler is still asked: before, the ZAP server dropped
        // such a request and every IPC client was refused.
        #[cfg(unix)]
        {
            let dir = std::env::temp_dir();
            let pid = std::process::id();
            let plain = dir.join(format!("monocoque-zap-plain-{pid}.sock"));
            let curve = dir.join(format!("monocoque-zap-curve-{pid}.sock"));

            handshake_pair_on(
                &format!("ipc://{}", plain.display()),
                SocketOptions::default()
                    .with_plain_server(true)
                    .with_zap_domain("test")
                    .with_handshake_timeout(timeout),
                SocketOptions::default()
                    .with_plain_credentials("alice", "secret")
                    .with_handshake_timeout(timeout),
            )
            .await;
            handshake_pair_on(
                &format!("ipc://{}", curve.display()),
                SocketOptions::default()
                    .with_curve_server(true)
                    .with_curve_keypair(server_public, server_secret)
                    .with_zap_domain("test")
                    .with_handshake_timeout(timeout),
                SocketOptions::default()
                    .with_curve_keypair(client_public, client_secret)
                    .with_curve_serverkey(server_public)
                    .with_handshake_timeout(timeout),
            )
            .await;
            let _ = std::fs::remove_file(plain);
            let _ = std::fs::remove_file(curve);

            let seen = handler.addresses.lock().unwrap().clone();
            assert_eq!(
                seen[2..],
                [String::new(), String::new()],
                "the IPC requests (PLAIN, then CURVE) carry an empty address"
            );
        }
    });
}
