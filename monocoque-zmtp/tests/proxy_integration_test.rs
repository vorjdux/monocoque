//! Integration tests for `proxy_steerable` with separate compio runtimes.
//!
//! Each test spawns its own compio Runtime in a dedicated OS thread to avoid
//! residual-timer crosstalk from prior handshake timeouts.
//!
//! Coordination between threads uses `std::sync::mpsc` channels.

use bytes::Bytes;
use compio::net::TcpListener;
use monocoque_zmtp::pair::PairSocket;
use monocoque_zmtp::proxy::{proxy_steerable, ProxyCommand};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Bind a TCP listener and return the address; used to set up a PAIR pair.
///
/// The caller is responsible for the accept and connect calls in the
/// appropriate runtime context.
async fn bind_listener() -> (TcpListener, std::net::SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

/// Connect two PairSockets over TCP and return (server_side, client_side).
async fn pair_connected() -> (PairSocket, PairSocket) {
    let (listener, addr) = bind_listener().await;
    let client_task = compio::runtime::spawn(PairSocket::connect(addr));
    let (stream, _) = listener.accept().await.unwrap();
    let server = PairSocket::from_tcp(stream).await.unwrap();
    let client = client_task.await.unwrap();
    (server, client)
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: TERMINATE command stops the proxy
// ─────────────────────────────────────────────────────────────────────────────

/// Start a PAIR→PAIR proxy (with a control socket), send "TERMINATE" on the
/// control socket, and verify the proxy exits cleanly.
#[test]
fn test_proxy_steerable_terminate() {
    let (result_tx, result_rx) = mpsc::channel::<bool>();

    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async move {
            // Set up three PAIR socket pairs: frontend, backend, control.
            let (frontend, mut client_a) = pair_connected().await;
            let (backend, mut client_b) = pair_connected().await;
            let (control, mut ctrl_client) = pair_connected().await;

            // Spawn proxy task inside the same runtime.
            let proxy_task = compio::runtime::spawn(async move {
                let mut fe = frontend;
                let mut be = backend;
                let mut ctrl = control;
                let capture: Option<&mut PairSocket> = None;
                proxy_steerable(&mut fe, &mut be, capture, &mut ctrl).await
            });

            // Send one message through the proxy to confirm it is running.
            client_a.send(vec![Bytes::from("ping")]).await.unwrap();
            let _msg = compio::time::timeout(Duration::from_secs(5), client_b.recv())
                .await
                .expect("forward timed out")
                .expect("io error")
                .expect("connection closed");

            // Send TERMINATE — proxy_steerable must return Ok(()).
            ctrl_client
                .send(vec![Bytes::from("TERMINATE")])
                .await
                .unwrap();

            let proxy_result =
                compio::time::timeout(Duration::from_secs(5), proxy_task)
                    .await
                    .expect("proxy did not exit within timeout");

            result_tx
                .send(proxy_result.is_ok())
                .unwrap();
        });
    });

    let ok = result_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(ok, "proxy_steerable should return Ok(()) on TERMINATE");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test: STATISTICS command replies with message counter
// ─────────────────────────────────────────────────────────────────────────────

/// Send a few messages through the proxy, then send "STATISTICS" on the control
/// socket and verify the reply starts with "messages_forwarded=".
#[test]
fn test_proxy_steerable_statistics() {
    let (stats_tx, stats_rx) = mpsc::channel::<String>();

    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async move {
            let (frontend, mut client_a) = pair_connected().await;
            let (backend, mut client_b) = pair_connected().await;
            let (control, mut ctrl_client) = pair_connected().await;

            let proxy_task = compio::runtime::spawn(async move {
                let mut fe = frontend;
                let mut be = backend;
                let mut ctrl = control;
                let capture: Option<&mut PairSocket> = None;
                proxy_steerable(&mut fe, &mut be, capture, &mut ctrl).await
            });

            // Forward a couple of messages so the counter is non-zero.
            for i in 0..2u32 {
                client_a
                    .send(vec![Bytes::from(format!("msg-{}", i))])
                    .await
                    .unwrap();
                let _msg = compio::time::timeout(Duration::from_secs(5), client_b.recv())
                    .await
                    .expect("forward timed out")
                    .expect("io error")
                    .expect("connection closed");
            }

            // Ask for statistics.
            ctrl_client
                .send(vec![Bytes::from("STATISTICS")])
                .await
                .unwrap();

            // The proxy sends the stats reply back on the same control socket.
            let stats_msg =
                compio::time::timeout(Duration::from_secs(5), ctrl_client.recv())
                    .await
                    .expect("statistics reply timed out")
                    .expect("io error")
                    .expect("connection closed");

            let reply = std::str::from_utf8(&stats_msg[0])
                .expect("non-UTF-8 stats reply")
                .to_owned();
            stats_tx.send(reply).unwrap();

            // Terminate cleanly.
            ctrl_client
                .send(vec![Bytes::from("TERMINATE")])
                .await
                .unwrap();
            let _ = compio::time::timeout(Duration::from_secs(5), proxy_task).await;
        });
    });

    let reply = stats_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        reply.starts_with("messages_forwarded="),
        "expected reply starting with 'messages_forwarded=', got: {:?}",
        reply
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Sanity: ProxyCommand byte parsing (no runtime needed)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_proxy_command_roundtrip() {
    let cases = [
        (b"PAUSE" as &[u8], ProxyCommand::Pause),
        (b"RESUME", ProxyCommand::Resume),
        (b"TERMINATE", ProxyCommand::Terminate),
        (b"STATISTICS", ProxyCommand::Statistics),
    ];
    for (bytes, cmd) in cases {
        assert_eq!(ProxyCommand::from_bytes(bytes), Some(cmd));
        assert_eq!(cmd.as_bytes(), bytes);
    }
    assert_eq!(ProxyCommand::from_bytes(b"UNKNOWN"), None);
}
