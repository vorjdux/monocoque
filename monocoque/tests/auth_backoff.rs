//! Repeated failed authentication from one address slows that address down:
//! after two free failures, its next handshakes wait before they start, and a
//! success clears the slate.
//!
//! The ZAP endpoint is a process-global inproc name, so this file holds the
//! only ZAP server bound in its process.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, SocketOptions};
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse};
use monocoque_zmtp::security::zap_handler::{ZapHandler, spawn_zap_server};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Accepts only the password "right".
struct OnePassword;

impl ZapHandler for OnePassword {
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        if request
            .credentials
            .get(1)
            .is_some_and(|p| &p[..] == b"right")
        {
            ZapResponse::success(request.request_id.clone(), "alice")
        } else {
            ZapResponse::failure(request.request_id.clone(), "no")
        }
    }
}

/// Connect with `password` and wait until the server admits the peer or
/// `patience` runs out; return how long admission took. Every client is kept
/// in `clients` so the server's peer count only grows.
async fn attempt(
    server: &DealerSocket,
    clients: &mut Vec<DealerSocket>,
    ep: &str,
    password: &str,
    patience: Duration,
) -> Option<Duration> {
    let before = server.peer_count();
    let mut client = DealerSocket::with_options(
        SocketOptions::default()
            .with_plain_credentials("alice", password)
            .with_reconnect_ivl(Duration::from_secs(60)),
    );
    let start = Instant::now();
    client.connect(ep).await.unwrap();
    clients.push(client);
    while start.elapsed() < patience {
        if server.peer_count() > before {
            return Some(start.elapsed());
        }
        monocoque::rt::sleep(Duration::from_millis(2)).await;
    }
    None
}

#[test]
fn failed_logins_delay_the_next_handshakes_from_that_address() {
    LocalRuntime::new().unwrap().block_on(async {
        spawn_zap_server(Arc::new(OnePassword)).unwrap();
        let mut server = DealerSocket::with_options(
            SocketOptions::default()
                .with_plain_server(true)
                .with_zap_domain("backoff")
                .with_handshake_timeout(Duration::from_secs(5)),
        );
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();

        let mut clients = Vec::new();
        let long = Duration::from_secs(2);

        // Unthrottled to begin with.
        let fast = attempt(&server, &mut clients, &ep, "right", long)
            .await
            .unwrap();
        assert!(
            fast < Duration::from_millis(80),
            "first login took {fast:?}"
        );

        // Four failures. The fourth already waits 100 ms; the next waits 200.
        for _ in 0..4 {
            let short = Duration::from_millis(300);
            assert!(
                attempt(&server, &mut clients, &ep, "wrong", short)
                    .await
                    .is_none()
            );
        }
        let slowed = attempt(&server, &mut clients, &ep, "right", long)
            .await
            .unwrap();
        assert!(
            slowed >= Duration::from_millis(150),
            "login after failures took {slowed:?}"
        );

        // That success cleared the address.
        let fast = attempt(&server, &mut clients, &ep, "right", long)
            .await
            .unwrap();
        assert!(
            fast < Duration::from_millis(80),
            "login after success took {fast:?}"
        );
    });
}
