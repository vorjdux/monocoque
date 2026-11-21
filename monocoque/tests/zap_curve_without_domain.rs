//! A CURVE server asks the ZAP handler about every client whenever a handler
//! is running, whether or not the server set a ZAP domain.
//!
//! Before, a server without a domain skipped ZAP altogether, so an allowlist
//! handler started beside it was never asked and every client key was let in.
//!
//! Each test binary is its own process and the ZAP endpoint is a process-global
//! inproc name, so this file holds the only ZAP server bound in its process.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::security::curve::CurveSecretKey;
use monocoque::zmq::security::zap::{ZapRequest, ZapResponse};
use monocoque::zmq::security::zap_handler::{ZapHandler, spawn_zap_server};
use monocoque::zmq::{DealerSocket, SocketOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Gate {
    allow: AtomicBool,
    asked: AtomicUsize,
}

impl ZapHandler for Gate {
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if self.allow.load(Ordering::SeqCst) {
            ZapResponse::success(request.request_id.clone(), "peer")
        } else {
            ZapResponse::failure(request.request_id.clone(), "not on the list")
        }
    }
}

fn keypair(seed: u8) -> ([u8; 32], [u8; 32]) {
    let secret = [seed; 32];
    let public = *CurveSecretKey::from_bytes(secret).public_key().as_bytes();
    (public, secret)
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        monocoque::rt::sleep(Duration::from_millis(2)).await;
    }
}

#[test]
fn a_running_handler_is_asked_without_a_zap_domain() {
    LocalRuntime::new().unwrap().block_on(async {
        let gate = Arc::new(Gate::default());
        spawn_zap_server(Arc::clone(&gate)).unwrap();

        let (server_public, server_secret) = keypair(1);
        let (client_public, client_secret) = keypair(2);
        let timeout = Duration::from_secs(5);

        // No `with_zap_domain` on the server.
        let mut server = DealerSocket::with_options(
            SocketOptions::default()
                .with_curve_server(true)
                .with_curve_keypair(server_public, server_secret)
                .with_handshake_timeout(timeout),
        );
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client = DealerSocket::with_options(
            SocketOptions::default()
                .with_curve_keypair(client_public, client_secret)
                .with_curve_serverkey(server_public)
                .with_reconnect_ivl(Duration::from_millis(10))
                .with_handshake_timeout(timeout),
        );
        client.connect(&ep).await.unwrap();

        // The handler says no: it is asked, and no peer comes up.
        wait_until("the handler to be asked twice", || {
            gate.asked.load(Ordering::SeqCst) >= 2
        })
        .await;
        assert_eq!(server.peer_count(), 0, "a refused client must not connect");

        // The handler says yes: the client's next dial is let in.
        gate.allow.store(true, Ordering::SeqCst);
        wait_until("the client to be admitted", || server.peer_count() == 1).await;
    });
}
