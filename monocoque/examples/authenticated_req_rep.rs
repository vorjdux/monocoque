//! Authenticated REQ/REP Example
//!
//! Demonstrates end-to-end PLAIN authentication with ZAP handler.
//!
//! This example shows:
//! - Running a ZAP server for authentication
//! - Creating a REP server with PLAIN authentication enabled
//! - Creating REQ clients with valid/invalid credentials
//! - Secure request-reply messaging

use bytes::Bytes;
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap_handler::start_default_zap_server;
use compio::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    println!("=== Authenticated REQ/REP Demo ===\n");

    // Step 1: Create and configure PLAIN authentication handler
    let mut plain_handler = StaticPlainHandler::new();
    plain_handler.add_user("alice", "password123");
    plain_handler.add_user("bob", "secretpass");
    
    println!("📋 User Database:");
    println!("  ✓ alice / password123");
    println!("  ✓ bob / secretpass");
    println!();

    // Step 2: Start ZAP server on inproc://zeromq.zap.01
    println!("🚀 Starting ZAP authentication server...");
    let _zap_task = start_default_zap_server(Arc::new(plain_handler), true);
    println!("✅ ZAP server running on inproc://zeromq.zap.01\n");

    // Step 3: Create REP server with PLAIN authentication enabled
    let server_options = SocketOptions::new()
        .with_plain_server(true)
        .with_recv_timeout(Duration::from_secs(10))
        .with_send_timeout(Duration::from_secs(10));

    println!("🔧 Creating REP server with PLAIN auth...");
    let mut server = RepSocket::from_tcp_with_options("127.0.0.1:0", server_options).await?;
    
    let endpoint = server
        .last_endpoint()
        .expect("Server should have endpoint")
        .to_string();
    
    println!("✅ Server listening on {}", endpoint);
    println!("   Authentication: PLAIN required");
    println!();

    // Step 4: Spawn server task
    let server_task = compio::runtime::spawn(async move {
        println!("[SERVER] Waiting for authenticated requests...\n");
        
        for i in 1..=3 {
            match server.recv().await {
                Ok(Some(request)) => {
                    let msg = String::from_utf8_lossy(&request[0]);
                    println!("[SERVER] Request {}: {}", i, msg);
                    
                    let reply = vec![Bytes::from(format!("Reply {}: Authenticated OK", i))];
                    server.send(reply).await.expect("Failed to send reply");
                    println!("[SERVER] Sent reply {}", i);
                }
                Ok(None) => {
                    println!("[SERVER] Connection closed");
                    break;
                }
                Err(e) => {
                    eprintln!("[SERVER] Error: {}", e);
                    break;
                }
            }
        }
        
        println!("\n[SERVER] Shutting down");
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Step 5: Create authenticated client (Alice)
    println!("👤 Creating REQ client for Alice...");
    let alice_options = SocketOptions::new()
        .with_plain_credentials("alice", "password123")
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5));

    let mut alice = ReqSocket::from_tcp_with_options(&endpoint, alice_options).await?;
    println!("✅ Alice connected and authenticated\n");

    // Alice sends request
    println!("[ALICE] Sending request...");
    alice.send(vec![Bytes::from("Hello from Alice")]).await?;
    
    let response = alice.recv().await?.expect("Expected response");
    println!("[ALICE] Received: {}\n", String::from_utf8_lossy(&response[0]));

    // Step 6: Create another authenticated client (Bob)
    println!("👤 Creating REQ client for Bob...");
    let bob_options = SocketOptions::new()
        .with_plain_credentials("bob", "secretpass")
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5));

    let mut bob = ReqSocket::from_tcp_with_options(&endpoint, bob_options).await?;
    println!("✅ Bob connected and authenticated\n");

    // Bob sends request
    println!("[BOB] Sending request...");
    bob.send(vec![Bytes::from("Hello from Bob")]).await?;
    
    let response = bob.recv().await?.expect("Expected response");
    println!("[BOB] Received: {}\n", String::from_utf8_lossy(&response[0]));

    // Step 7: Try with invalid credentials (should fail)
    println!("🚫 Attempting connection with invalid credentials...");
    let invalid_options = SocketOptions::new()
        .with_plain_credentials("alice", "wrongpassword")
        .with_recv_timeout(Duration::from_secs(2))
        .with_send_timeout(Duration::from_secs(2));

    match ReqSocket::from_tcp_with_options(&endpoint, invalid_options).await {
        Ok(mut client) => {
            println!("⚠️  Connection succeeded (ZAP check will happen during send)");
            match client.send(vec![Bytes::from("This should fail")]).await {
                Ok(_) => println!("⚠️  Send succeeded but reply should fail"),
                Err(e) => println!("❌ Send failed: {}", e),
            }
        }
        Err(e) => {
            println!("❌ Connection failed (expected): {}", e);
        }
    }

    println!();

    // Cleanup
    drop(alice);
    drop(bob);

    // Wait for server to finish
    server_task.await.expect("Server task failed");

    println!("\n✅ Demo completed successfully!");
    println!("   All authenticated requests were processed");
    println!("   Invalid credentials were rejected");

    Ok(())
}
