//! Demonstrates socket lifecycle event monitoring.
//!
//! This example shows how to create and use a socket monitor to track
//! connection events like connects, disconnects, binds, etc.

use monocoque::zmq::{SocketEvent, SocketMonitor};
use std::time::Duration;

#[compio::main]
async fn main() -> std::io::Result<()> {
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
    let tcp_ep = monocoque::zmq::Endpoint::parse("tcp://127.0.0.1:5555").unwrap();
    let bind_ep = monocoque::zmq::Endpoint::parse("tcp://0.0.0.0:6666").unwrap();
    let peer_ep = monocoque::zmq::Endpoint::parse("tcp://192.168.1.100:12345").unwrap();
    
    println!("  {}", SocketEvent::Connected(tcp_ep.clone()));
    println!("  {}", SocketEvent::Bound(bind_ep.clone()));
    println!("  {}", SocketEvent::Listening(bind_ep.clone()));
    println!("  {}", SocketEvent::Accepted(peer_ep.clone()));
    println!("  {}", SocketEvent::Disconnected(tcp_ep.clone()));
    println!("  {}", SocketEvent::BindFailed { 
        endpoint: bind_ep.clone(), 
        reason: "Address already in use".to_string() 
    });
    println!("  {}", SocketEvent::ConnectFailed { 
        endpoint: tcp_ep.clone(), 
        reason: "Connection refused".to_string() 
    });
    
    // Monitor events in background task
    let _monitor_task = compio::runtime::spawn(async move {
        while let Ok(event) = monitor.recv_async().await {
            println!("📡 Socket Event: {}", event);
        }
    });
    
    compio::time::sleep(Duration::from_millis(100)).await;
    
    println!("\n✅ Socket monitoring example completed");
    Ok(())
}

fn create_example_monitor() -> SocketMonitor {
    let (sender, receiver) = flume::unbounded();
    
    // Simulate some events
    let tcp_ep = monocoque::zmq::Endpoint::parse("tcp://127.0.0.1:5555").unwrap();
    let _ = sender.send(SocketEvent::Connected(tcp_ep.clone()));
    let _ = sender.send(SocketEvent::Disconnected(tcp_ep));
    
    receiver
}

async fn simulate_socket_lifecycle() {
    println!("Simulating socket lifecycle:");
    println!("  1. Socket created");
    compio::time::sleep(Duration::from_millis(10)).await;
    
    println!("  2. Binding to address...");
    compio::time::sleep(Duration::from_millis(10)).await;
    
    println!("  3. Listening for connections...");
    compio::time::sleep(Duration::from_millis(10)).await;
    
    println!("  4. Connection accepted");
    compio::time::sleep(Duration::from_millis(10)).await;
    
    println!("  5. Connection closed");
}
