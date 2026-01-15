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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info};

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== PubSub Events Example ===\n");

    // Shared port between publisher and subscriber
    let port = Arc::new(Mutex::new(None));
    let port_clone = port.clone();

    // Start publisher in background
    let publisher_handle = compio::runtime::spawn(async move {
        if let Err(e) = run_publisher(port_clone).await {
            error!("[Main] Publisher error: {e}");
        }
    });

    // Give publisher time to bind
    compio::time::sleep(Duration::from_millis(500)).await;

    // Get the port
    let port_num = {
        let p = port.lock().unwrap();
        p.expect("Publisher should have set port")
    };

    // Start subscriber in background BEFORE publisher accepts
    let subscriber_handle = compio::runtime::spawn(async move {
        if let Err(e) = run_subscriber(port_num).await {
            error!("[Main] Subscriber error: {e}");
        }
    });
    
    // Give subscriber time to connect and subscribe
    compio::time::sleep(Duration::from_millis(500)).await;

    // Wait for both to complete
    let _ = futures::join!(publisher_handle, subscriber_handle);

    Ok(())
}

async fn run_publisher(port: Arc<Mutex<Option<u16>>>) -> Result<(), Box<dyn std::error::Error>> {
    info!("[Publisher] Starting...");

    let mut socket = PubSocket::bind("127.0.0.1:0").await?;
    let bound_port = socket.local_addr()?.port();
    
    // Share the port with subscriber
    *port.lock().unwrap() = Some(bound_port);
    info!("[Publisher] Bound to port {}", bound_port);

    // Accept subscriber connection
    socket.accept_subscriber().await?;
    info!("[Publisher] Subscriber connected");

    // Give subscriber time to send subscription
    std::thread::sleep(Duration::from_millis(500));

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

        match socket.send(message).await {
            Ok(()) => {}
            Err(e) => {
                error!("[Publisher] Send error: {e}");
                break;
            }
        }
        
        // Small delay between messages
        std::thread::sleep(Duration::from_millis(50));
    }

    info!("[Publisher] Done publishing");

    // Keep connection alive briefly
    std::thread::sleep(Duration::from_millis(200));
    
    Ok(())
}

async fn run_subscriber(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    info!("[Subscriber] Connecting to publisher on port {}...", port);

    let mut socket = SubSocket::connect(&format!("127.0.0.1:{}", port)).await?;

    // Subscribe to trade events only
    info!("[Subscriber] Subscribing to 'trade.' prefix");
    socket.subscribe(b"trade.").await?;
    
    // Small delay to ensure subscription is registered before messages are sent
    std::thread::sleep(Duration::from_millis(50));

    info!("[Subscriber] Waiting for events...\n");

    // Receive events
    for _ in 0..10 {
        match socket.recv().await {
            Ok(Some(message)) => {
                if message.len() >= 2 {
                    let topic = std::str::from_utf8(&message[0]).unwrap_or("<invalid>");
                    let data = std::str::from_utf8(&message[1]).unwrap_or("<invalid>");
                    info!("[Subscriber] Received: {topic} -> {data}");
                } else {
                    info!(
                        "[Subscriber] Received message with {} frames",
                        message.len()
                    );
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
