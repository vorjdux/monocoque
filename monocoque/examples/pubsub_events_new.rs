/// PubSub Events Example
///
/// This example demonstrates PUB/SUB pattern for event distribution:
/// - Publisher broadcasts events on different topics using worker pool
/// - Subscriber filters events by topic prefix
///
/// Architecture:
/// - PUB socket with worker pool broadcasts to all subscribers
/// - SUB socket subscribes to specific topics
/// - Topics are prefix-matched (e.g., "trade." matches "trade.BTC", "trade.ETH")

use bytes::Bytes;
use monocoque::zmq::{PubSocket, SubSocket};
use std::thread;
use std::time::Duration;
use tracing::{error, info};

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== PubSub Events Example ===\n");

    // Start publisher
    let mut pub_socket = PubSocket::bind("127.0.0.1:5558").await?;
    info!("[Publisher] Bound to 127.0.0.1:5558");

    // Start subscriber in background
    compio::runtime::spawn(async {
        run_subscriber().await.ok();
    }).detach();

    // Give subscriber time to connect
    thread::sleep(Duration::from_millis(500));

    // Accept subscriber connection
    pub_socket.accept_subscriber().await?;
    info!("[Publisher] Subscriber connected");

    // Give subscriber time to send subscription
    thread::sleep(Duration::from_secs(2));

    info!("[Publisher] Publishing events...");

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
        let message = vec![Bytes::from(topic), Bytes::from(data)];
        info!("[Publisher] Publishing: {topic} -> {data}");
        pub_socket.send(message).await?;
        thread::sleep(Duration::from_millis(100));
    }

    info!("[Publisher] Done publishing");

    // Keep connection alive briefly for subscribers to receive
    thread::sleep(Duration::from_secs(1));

    Ok(())
}

async fn run_subscriber() -> Result<(), Box<dyn std::error::Error>> {
    info!("[Subscriber] Task started");
    info!("[Subscriber] Connecting to publisher on port 5558...");
    let mut socket = SubSocket::connect("127.0.0.1:5558").await?;
    info!("[Subscriber] Connected!");

    // Subscribe to trade events only
    info!("[Subscriber] Subscribing to 'trade.' prefix");
    socket.subscribe(b"trade.").await?;
    info!("[Subscriber] Subscribed!");

    info!("[Subscriber] Waiting for events...\n");

    // Receive events (should get 4 trade.* messages out of 6 total)
    for _ in 0..4 {
        match socket.recv().await {
            Ok(Some(message)) => {
                if message.len() >= 2 {
                    let topic = std::str::from_utf8(&message[0]).unwrap_or("<invalid>");
                    let data = std::str::from_utf8(&message[1]).unwrap_or("<invalid>");
                    info!("[Subscriber] Received: {topic} -> {data}");
                }
            }
            Ok(None) => {
                info!("[Subscriber] Connection closed");
                break;
            }
            Err(e) => {
                error!("[Subscriber] Recv error: {e}");
                break;
            }
        }
    }

    info!("[Subscriber] Done receiving");
    Ok(())
}
