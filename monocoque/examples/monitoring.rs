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
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, Msg, RouterSocket, SocketEvent, SocketMonitor};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

/// Print every event `monitor` delivers until its socket is gone.
fn watch(name: &'static str, monitor: SocketMonitor) {
    rt::spawn_detached(async move {
        while let Ok(event) = monitor.recv_async().await {
            match event {
                SocketEvent::Connected(ep) => {
                    println!("  [{name}] Connected to {ep}");
                }
                SocketEvent::Disconnected(ep) => {
                    println!("  [{name}] Disconnected from {ep}");
                }
                SocketEvent::ConnectFailed { endpoint, reason } => {
                    println!("  [{name}] Connection failed for {endpoint}: {reason}");
                }
                SocketEvent::Bound(ep) => {
                    println!("  [{name}] Bound to {ep}");
                }
                SocketEvent::BindFailed { endpoint, reason } => {
                    println!("  [{name}] Bind failed for {endpoint}: {reason}");
                }
                SocketEvent::Listening(ep) => {
                    println!("  [{name}] Listening on {ep}");
                }
                SocketEvent::Accepted(ep) => {
                    println!("  [{name}] Accepted connection from {ep}");
                }
            }
        }
        println!("  [{name}] Monitoring ended");
    });
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Socket Monitoring Example ===\n");

    // Enable monitoring before binding or connecting so no event is missed
    let mut router = RouterSocket::new();
    watch("ROUTER", router.monitor());
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;

    let mut socket = DealerSocket::new();
    watch("DEALER", socket.monitor());
    socket.connect(&endpoint).await?;

    println!("Created ROUTER and DEALER with monitoring enabled");
    println!("  Endpoint: {endpoint}\n");

    // Simulate some socket operations
    println!("Sending test message...");
    socket
        .send(Msg::from([Bytes::from("Hello"), Bytes::from("World")]))
        .await?;
    let msg = router.recv_owned().await?;
    println!("ROUTER received {} parts (identity included)\n", msg.len());

    // Close the server to trigger disconnect events on the DEALER
    println!("Closing the ROUTER...");
    router.close().await?;
    rt::sleep(Duration::from_millis(100)).await;

    // Drop the DEALER; its monitor channel closes with it
    drop(socket);
    rt::sleep(Duration::from_millis(100)).await;

    println!("\n=== Example Complete ===");

    Ok(())
}
