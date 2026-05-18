//! Authenticated REQ/REP Example
//!
//! Demonstrates end-to-end PLAIN authentication with ZAP handler.
//!
//! This example shows:
//! - Running a ZAP server for authentication
//! - Creating a REP server with PLAIN authentication enabled
//! - Creating REQ clients with valid credentials
//! - Secure request-reply messaging

use bytes::Bytes;
use compio::net::TcpListener;
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap_handler::start_default_zap_server;
use std::sync::Arc;
use std::time::Duration;

#[compio::main]
async fn main() -> std::io::Result<()> {
    println!("=== Authenticated REQ/REP Demo ===\n");

    let mut plain_handler = StaticPlainHandler::new();
    plain_handler.add_user("alice", "password123");
    plain_handler.add_user("bob", "secretpass");

    println!("User Database: alice / password123, bob / secretpass");

    start_default_zap_server(Arc::new(plain_handler), false)?;
    println!("ZAP server running\n");

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;
    println!("Server listening on {}", local_addr);

    let server_options = SocketOptions::new()
        .with_plain_server(true)
        .with_recv_timeout(Duration::from_secs(10))
        .with_send_timeout(Duration::from_secs(10));

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut server = RepSocket::from_tcp_with_options(stream, server_options)
            .await
            .unwrap();

        println!("[SERVER] Waiting for requests...");
        for i in 1..=2 {
            if let Ok(Some(request)) = server.recv().await {
                let msg = String::from_utf8_lossy(&request[0]);
                println!("[SERVER] Request {}: {}", i, msg);
                let reply = vec![Bytes::from(format!("Reply {}: OK", i))];
                server.send(reply).await.unwrap();
            }
        }
        println!("[SERVER] Done");
    });

    compio::time::sleep(Duration::from_millis(50)).await;

    let alice_options = SocketOptions::new()
        .with_plain_credentials("alice", "password123")
        .with_recv_timeout(Duration::from_secs(5));

    let mut alice = ReqSocket::connect_with_options(&local_addr.to_string(), alice_options).await?;
    println!("[ALICE] Connected");

    alice.send(vec![Bytes::from("Hello from Alice")]).await?;
    if let Ok(Some(response)) = alice.recv().await {
        println!(
            "[ALICE] Received: {}",
            String::from_utf8_lossy(&response[0])
        );
    }

    let bob_options = SocketOptions::new()
        .with_plain_credentials("bob", "secretpass")
        .with_recv_timeout(Duration::from_secs(5));

    let mut bob = ReqSocket::connect_with_options(&local_addr.to_string(), bob_options).await?;
    println!("[BOB] Connected");

    bob.send(vec![Bytes::from("Hello from Bob")]).await?;
    if let Ok(Some(response)) = bob.recv().await {
        println!("[BOB] Received: {}", String::from_utf8_lossy(&response[0]));
    }

    server_task.await;
    println!("\nDemo completed!");
    Ok(())
}
