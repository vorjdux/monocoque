//! Integration tests for the proxy message-forwarding functionality.
//!
//! Tests use PAIR sockets for simplicity since they support bidirectional
//! communication. The steerable variant is used so tests can send TERMINATE
//! to cleanly exit the proxy loop.

use bytes::Bytes;
use monocoque_core::rt::TcpListener;
use monocoque_zmtp::pair::PairSocket;
use monocoque_zmtp::proxy::{ProxyCommand, proxy_steerable};

/// Bind a TCP listener and return a connected server+client PAIR socket pair.
#[allow(clippy::future_not_send)]
async fn pair_connected() -> (PairSocket, PairSocket) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Spawn the connect side so it runs concurrently with the accept+handshake below.
    let client_task = monocoque_core::rt::spawn(PairSocket::connect(addr));
    let (stream, _) = listener.accept().await.unwrap();
    let server = PairSocket::from_tcp(stream).await.unwrap();
    let client = monocoque_core::rt::join(client_task).await.unwrap();
    (server, client)
}

/// `ProxyCommand` byte parsing is a pure function  -  no runtime needed.
#[test]
fn test_proxy_command_parsing() {
    assert_eq!(
        ProxyCommand::from_bytes(b"PAUSE"),
        Some(ProxyCommand::Pause)
    );
    assert_eq!(
        ProxyCommand::from_bytes(b"RESUME"),
        Some(ProxyCommand::Resume)
    );
    assert_eq!(
        ProxyCommand::from_bytes(b"TERMINATE"),
        Some(ProxyCommand::Terminate)
    );
    assert_eq!(
        ProxyCommand::from_bytes(b"STATISTICS"),
        Some(ProxyCommand::Statistics)
    );
    assert_eq!(ProxyCommand::from_bytes(b"UNKNOWN"), None);
    assert_eq!(ProxyCommand::from_bytes(b""), None);
}

/// Proxy forwards a message from the frontend-side client to the backend-side client.
#[test]
fn test_proxy_pair_forward() {
    monocoque_core::rt::LocalRuntime::new()
        .unwrap()
        .block_on(test_proxy_pair_forward_impl());
}

async fn test_proxy_pair_forward_impl() {
    let (frontend, mut client_a) = pair_connected().await;
    let (backend, mut client_b) = pair_connected().await;
    let (control, mut ctrl_client) = pair_connected().await;

    let proxy_task = monocoque_core::rt::spawn(async move {
        let mut fe = frontend;
        let mut be = backend;
        let mut ctrl = control;
        let capture: Option<&mut PairSocket> = None;
        proxy_steerable(&mut fe, &mut be, capture, &mut ctrl).await
    });

    client_a.send(vec![Bytes::from("hello")]).await.unwrap();
    let msg = client_b.recv().await.unwrap().unwrap();
    assert_eq!(msg, vec![Bytes::from("hello")]);

    ctrl_client
        .send(vec![Bytes::from("TERMINATE")])
        .await
        .unwrap();
    monocoque_core::rt::join(proxy_task).await.unwrap();
}

/// Proxy forwards messages in both directions (frontend→backend and backend→frontend).
#[test]
fn test_proxy_pair_bidirectional() {
    monocoque_core::rt::LocalRuntime::new()
        .unwrap()
        .block_on(test_proxy_pair_bidirectional_impl());
}

async fn test_proxy_pair_bidirectional_impl() {
    let (frontend, mut client_a) = pair_connected().await;
    let (backend, mut client_b) = pair_connected().await;
    let (control, mut ctrl_client) = pair_connected().await;

    let proxy_task = monocoque_core::rt::spawn(async move {
        let mut fe = frontend;
        let mut be = backend;
        let mut ctrl = control;
        let capture: Option<&mut PairSocket> = None;
        proxy_steerable(&mut fe, &mut be, capture, &mut ctrl).await
    });

    // Frontend-side → backend-side
    client_a.send(vec![Bytes::from("A to B")]).await.unwrap();
    let msg = client_b.recv().await.unwrap().unwrap();
    assert_eq!(msg, vec![Bytes::from("A to B")]);

    // Backend-side → frontend-side
    client_b.send(vec![Bytes::from("B to A")]).await.unwrap();
    let msg = client_a.recv().await.unwrap().unwrap();
    assert_eq!(msg, vec![Bytes::from("B to A")]);

    ctrl_client
        .send(vec![Bytes::from("TERMINATE")])
        .await
        .unwrap();
    monocoque_core::rt::join(proxy_task).await.unwrap();
}

/// Capture socket receives a copy of every message the proxy forwards.
#[test]
fn test_proxy_capture_socket() {
    monocoque_core::rt::LocalRuntime::new()
        .unwrap()
        .block_on(test_proxy_capture_socket_impl());
}

async fn test_proxy_capture_socket_impl() {
    let (frontend, mut client_a) = pair_connected().await;
    let (backend, mut client_b) = pair_connected().await;
    let (control, mut ctrl_client) = pair_connected().await;
    let (capture_server, mut capture_client) = pair_connected().await;

    let proxy_task = monocoque_core::rt::spawn(async move {
        let mut fe = frontend;
        let mut be = backend;
        let mut ctrl = control;
        let mut cap = capture_server;
        proxy_steerable(&mut fe, &mut be, Some(&mut cap), &mut ctrl).await
    });

    // Proxy sends capture copy before forwarding to backend, so both arrive.
    client_a.send(vec![Bytes::from("captured")]).await.unwrap();

    let msg_b = client_b.recv().await.unwrap().unwrap();
    assert_eq!(msg_b, vec![Bytes::from("captured")]);

    let msg_cap = capture_client.recv().await.unwrap().unwrap();
    assert_eq!(msg_cap, vec![Bytes::from("captured")]);

    ctrl_client
        .send(vec![Bytes::from("TERMINATE")])
        .await
        .unwrap();
    monocoque_core::rt::join(proxy_task).await.unwrap();
}

/// TERMINATE command stops the proxy after it has forwarded at least one message.
#[test]
fn test_proxy_steerable_terminate() {
    monocoque_core::rt::LocalRuntime::new()
        .unwrap()
        .block_on(test_proxy_steerable_terminate_impl());
}

async fn test_proxy_steerable_terminate_impl() {
    let (frontend, mut client_a) = pair_connected().await;
    let (backend, mut client_b) = pair_connected().await;
    let (control, mut ctrl_client) = pair_connected().await;

    let proxy_task = monocoque_core::rt::spawn(async move {
        let mut fe = frontend;
        let mut be = backend;
        let mut ctrl = control;
        let capture: Option<&mut PairSocket> = None;
        proxy_steerable(&mut fe, &mut be, capture, &mut ctrl).await
    });

    // Send one message to confirm the proxy is running.
    client_a.send(vec![Bytes::from("ping")]).await.unwrap();
    client_b.recv().await.unwrap().unwrap();

    // TERMINATE should cause proxy_steerable to return Ok(()).
    ctrl_client
        .send(vec![Bytes::from("TERMINATE")])
        .await
        .unwrap();
    let result = monocoque_core::rt::join(proxy_task).await;
    assert!(
        result.is_ok(),
        "proxy_steerable should return Ok(()) on TERMINATE"
    );
}
