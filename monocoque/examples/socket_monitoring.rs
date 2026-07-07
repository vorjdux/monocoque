//! Demonstrates socket lifecycle event monitoring.
//!
//! This example shows how to create and use a socket monitor to track
//! connection events like connects, disconnects, binds, etc.

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{SocketEvent, SocketMonitor};
use monocoque_core::endpoint::Endpoint;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> std::io::Result<()> {
    // Create a monitor channel for tracking socket events
    let monitor = create_example_monitor();

    println!("Socket Monitor Example");
    println!("======================\n");

    // Simulate socket lifecycle events
    simulate_socket_lifecycle().await;

    // In a real application, you would:
    // 1. Create a socket
    // 2. Get its monitor: let monitor = socket.monitor();
    // 3. Spawn a task to handle events

    println!("\nExample event types:");
    let tcp_ep = Endpoint::parse("tcp://127.0.0.1:5555").unwrap();
    let bind_ep = Endpoint::parse("tcp://0.0.0.0:6666").unwrap();
    let peer_ep = Endpoint::parse("tcp://192.168.1.100:12345").unwrap();

    println!("  {}", SocketEvent::Connected(tcp_ep.clone()));
    println!("  {}", SocketEvent::Bound(bind_ep.clone()));
    println!("  {}", SocketEvent::Listening(bind_ep.clone()));
    println!("  {}", SocketEvent::Accepted(peer_ep.clone()));
    println!("  {}", SocketEvent::Disconnected(tcp_ep.clone()));
    println!(
        "  {}",
        SocketEvent::BindFailed {
            endpoint: bind_ep.clone(),
            reason: "Address already in use".to_string()
        }
    );
    println!(
        "  {}",
        SocketEvent::ConnectFailed {
            endpoint: tcp_ep.clone(),
            reason: "Connection refused".to_string()
        }
    );

    // Monitor events in background task
    rt::spawn_detached(async move {
        while let Ok(event) = monitor.recv_async().await {
            println!("📡 Socket Event: {event}");
        }
    });

    rt::sleep(Duration::from_millis(100)).await;

    println!("\n✅ Socket monitoring example completed");
    Ok(())
}

fn create_example_monitor() -> SocketMonitor {
    let (sender, receiver) = flume::unbounded();

    // Simulate some events
    let tcp_ep = Endpoint::parse("tcp://127.0.0.1:5555").unwrap();
    let _ = sender.send(SocketEvent::Connected(tcp_ep.clone()));
    let _ = sender.send(SocketEvent::Disconnected(tcp_ep));

    receiver
}

#[allow(clippy::future_not_send)]
async fn simulate_socket_lifecycle() {
    println!("Simulating socket lifecycle:");
    println!("  1. Socket created");
    rt::sleep(Duration::from_millis(10)).await;

    println!("  2. Binding to address...");
    rt::sleep(Duration::from_millis(10)).await;

    println!("  3. Listening for connections...");
    rt::sleep(Duration::from_millis(10)).await;

    println!("  4. Connection accepted");
    rt::sleep(Duration::from_millis(10)).await;

    println!("  5. Connection closed");
}
