//! Authenticated REQ/REP Example
//!
//! Demonstrates end-to-end PLAIN authentication with ZAP handler.
//!
//! This example shows:
//! - Running a ZAP server for authentication
//! - Creating a REP server with PLAIN authentication enabled
//! - Creating REQ clients with valid credentials
//! - Secure request-reply messaging

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap_handler::start_default_zap_server;
use std::sync::Arc;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Authenticated REQ/REP Demo ===\n");

    let mut plain_handler = StaticPlainHandler::new();
    plain_handler.add_user("alice", "password123");
    plain_handler.add_user("bob", "secretpass");

    println!("User Database: alice / password123, bob / secretpass");

    start_default_zap_server(Arc::new(plain_handler), false)?;
    println!("ZAP server running\n");

    let server_options = SocketOptions::new()
        .with_plain_server(true)
        .with_recv_timeout(Duration::from_secs(10))
        .with_send_timeout(Duration::from_secs(10));

    let mut server = RepSocket::with_options(server_options);
    let endpoint = server.bind("tcp://127.0.0.1:0").await?;
    println!("Server listening on {endpoint}");

    let server_task = rt::spawn(async move {
        // One bound socket authenticates every peer that connects; alice and
        // bob each get their own connection, and REP answers whoever asked.
        println!("[SERVER] Waiting for requests...");
        for i in 1..=2 {
            match server.recv().await {
                Ok(request) => {
                    let msg = String::from_utf8_lossy(request.first().unwrap_or_default());
                    println!("[SERVER] Request {i}: {msg}");
                }
                Err(e) => {
                    println!("[SERVER] Receive failed: {e}");
                    return;
                }
            }
            server.send(format!("Reply {i}: OK")).await.unwrap();
        }
        server.close().await.unwrap();
        println!("[SERVER] Done");
    });

    let alice_options = SocketOptions::new()
        .with_plain_credentials("alice", "password123")
        .with_recv_timeout(Duration::from_secs(5));

    let mut alice = ReqSocket::with_options(alice_options);
    alice.connect(&endpoint).await?;
    println!("[ALICE] Connecting");

    alice.send("Hello from Alice").await?;
    {
        let response = alice.recv().await?;
        println!(
            "[ALICE] Received: {}",
            String::from_utf8_lossy(response.first().unwrap_or_default())
        );
    }

    let bob_options = SocketOptions::new()
        .with_plain_credentials("bob", "secretpass")
        .with_recv_timeout(Duration::from_secs(5));

    let mut bob = ReqSocket::with_options(bob_options);
    bob.connect(&endpoint).await?;
    println!("[BOB] Connecting");

    bob.send("Hello from Bob").await?;
    {
        let response = bob.recv().await?;
        println!(
            "[BOB] Received: {}",
            String::from_utf8_lossy(response.first().unwrap_or_default())
        );
    }

    rt::join(server_task).await;
    println!("\nDemo completed!");
    Ok(())
}
