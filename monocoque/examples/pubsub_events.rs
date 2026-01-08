/// `PubSub` Events Example
///
/// This example demonstrates PUB/SUB pattern for event distribution:
/// - Publisher broadcasts events on different topics
/// - Subscribers filter events by topic prefix
///
/// Architecture:
/// - PUB socket broadcasts to all subscribers
/// - SUB socket subscribes to specific topics
/// - Topics are prefix-matched (e.g., "trade." matches "trade.BTC", "trade.ETH")

use bytes::Bytes;
use monocoque::zmq::{PubSocket, SubSocket};
use compio::net::{TcpListener, TcpStream};
use std::time::Duration;
use tracing::{info, error};

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    info!("=== PubSub Events Example ===\n");
    
    // Start subscriber in background
    let subscriber_handle = compio::runtime::spawn(async {
        run_subscriber().await;
    });
    
    // Give subscriber time to connect
    compio::time::sleep(Duration::from_millis(500)).await;
    
    // Start publisher
    let publisher_handle = compio::runtime::spawn(async {
        run_publisher().await;
    });
    
    // Wait for both to complete
    let _ = futures::join!(subscriber_handle, publisher_handle);
    
    Ok(())
}

async fn run_publisher() {
    info!("[Publisher] Starting on port 5556...");
    
    let listener = TcpListener::bind("127.0.0.1:5556").await.unwrap();
    
    // Accept subscriber connection
    let (stream, addr) = listener.accept().await.unwrap();
    info!("[Publisher] Subscriber connected from {addr}");
    
    let mut socket = PubSocket::from_stream(stream).await.unwrap();
    
    // Give handshake time to complete
    compio::time::sleep(Duration::from_millis(200)).await;
    
    // Publish events on different topics
    let events = vec![
        ("trade.BTC", "BTC/USD: 45000"),
        ("trade.ETH", "ETH/USD: 3000"),
        ("news.crypto", "New regulation announced"),
        ("trade.BTC", "BTC/USD: 45100"),
        ("alert.system", "System maintenance in 1 hour"),
        ("trade.ETH", "ETH/USD: 3050"),
    ];
    
    for (topic, data) in events {
        let message = vec![
            Bytes::from(topic),
            Bytes::from(data),
        ];
        
        info!("[Publisher] Publishing: {topic} -> {data}");
        
        match socket.send(message).await {
            Ok(()) => {}
            Err(e) => {
                error!("[Publisher] Send error: {e}");
                break;
            }
        }
        
        compio::time::sleep(Duration::from_millis(500)).await;
    }
    
    info!("[Publisher] Done publishing");
    
    // Keep connection alive briefly
    compio::time::sleep(Duration::from_secs(1)).await;
}

async fn run_subscriber() {
    // Wait for publisher to be ready
    compio::time::sleep(Duration::from_millis(200)).await;
    
    info!("[Subscriber] Connecting to publisher on port 5556...");
    
    let stream = TcpStream::connect("127.0.0.1:5556").await.unwrap();
    let mut socket = SubSocket::from_stream(stream).await.unwrap();
    
    // Subscribe to trade events only
    info!("[Subscriber] Subscribing to 'trade.' prefix");
    socket.subscribe(Bytes::from_static(b"trade."));
    
    // Give subscription time to register
    compio::time::sleep(Duration::from_millis(200)).await;
    
    info!("[Subscriber] Waiting for events...\n");
    
    // Receive events
    for _ in 0..10 {
        match compio::time::timeout(
            Duration::from_secs(2),
            socket.recv()
        ).await {
            Ok(Ok(Some(message))) => {
                if message.len() >= 2 {
                    let topic = std::str::from_utf8(&message[0]).unwrap_or("<invalid>");
                    let data = std::str::from_utf8(&message[1]).unwrap_or("<invalid>");
                    info!("[Subscriber] Received: {topic} -> {data}");
                } else {
                    info!("[Subscriber] Received message with {} frames", message.len());
                }
            }
                Ok(Ok(None)) => {
                    info!("[Subscriber] Connection closed");
                    break;
                }
                Ok(Err(e)) => {
                    error!("[Subscriber] Recv error: {e}");
                    break;
                }
                Err(_) => {
                    info!("[Subscriber] Timeout waiting for events");
                    break;
                }
        }
    }
    
    info!("[Subscriber] Done receiving");
}
