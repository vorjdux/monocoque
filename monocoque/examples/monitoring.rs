//! Example demonstrating socket monitoring capabilities.
//!
//! This example shows how to:
//! - Enable monitoring on a socket
//! - Receive connection events
//! - Handle socket lifecycle notifications
//!
//! Run with:
//! ```bash
//! cargo run --example monitoring --features zmq
//! ```

use bytes::Bytes;
use monocoque::zmq::{DealerSocket, SocketEvent};
use std::time::Duration;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Socket Monitoring Example ===\n");

    // Create a socket and enable monitoring
    let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
    let monitor = socket.monitor();

    println!("✓ Created DEALER socket with monitoring enabled");
    println!("  Endpoint: tcp://127.0.0.1:5555\n");

    // Spawn a task to handle monitoring events
    let _monitor_task = compio::runtime::spawn(async move {
        println!("📡 Monitoring task started...\n");
        
        while let Ok(event) = monitor.recv_async().await {
            match event {
                SocketEvent::Connected(ep) => {
                    println!("✓ Connected to {}", ep);
                }
                SocketEvent::Disconnected(ep) => {
                    println!("✗ Disconnected from {}", ep);
                }
                SocketEvent::ConnectFailed { endpoint, reason } => {
                    println!("✗ Connection failed for {}: {}", endpoint, reason);
                }
                SocketEvent::Bound(ep) => {
                    println!("✓ Bound to {}", ep);
                }
                SocketEvent::BindFailed { endpoint, reason } => {
                    println!("✗ Bind failed for {}: {}", endpoint, reason);
                }
                SocketEvent::Listening(ep) => {
                    println!("✓ Listening on {}", ep);
                }
                SocketEvent::Accepted(ep) => {
                    println!("✓ Accepted connection from {}", ep);
                }
            }
        }
        
        println!("\n📡 Monitoring task ended");
    });

    // Simulate some socket operations
    println!("Sending test message...");
    match socket
        .send(vec![Bytes::from("Hello"), Bytes::from("World")])
        .await
    {
        Ok(_) => println!("✓ Message sent successfully\n"),
        Err(e) => println!("✗ Failed to send message: {}\n", e),
    }

    // Wait a bit to see events
    compio::time::sleep(Duration::from_millis(100)).await;

    // Try to receive (will likely timeout since no server is running)
    println!("Attempting to receive...");
    if let Some(msg) = socket.recv().await {
        println!("✓ Received {} parts\n", msg.len());
    } else {
        println!("✗ No message received (connection closed or no server)\n");
    }

    // Drop the socket to trigger disconnect events
    drop(socket);

    // Wait for monitoring task to process final events
    compio::time::sleep(Duration::from_millis(100)).await;

    println!("\n=== Example Complete ===");
    println!("Note: Connect to an actual server to see full event flow");

    Ok(())
}
