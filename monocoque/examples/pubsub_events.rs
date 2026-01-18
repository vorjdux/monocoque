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
use std::time::Duration;
use tracing::{error, info};

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== PubSub Events Example ===\n");

    // Pick a port to use
    let port = portpicker::pick_unused_port().expect("No ports available");
    info!("Using port {}", port);

    // Start publisher
    let mut pub_socket = PubSocket::bind(format!("127.0.0.1:{}", port)).await?;
    info!("[Publisher] Bound to port {}", port);

    // Start subscriber in background FIRST (before accept)
    let subscriber_handle = compio::runtime::spawn(async move {
        if let Err(e) = run_subscriber(port).await {
            error!("[Subscriber] Error: {e}");
        }
    });

    // Small delay to allow subscriber to start connecting
    std::thread::sleep(Duration::from_millis(100));

    // Accept subscriber connection (this will block until subscriber connects)
    pub_socket.accept_subscriber().await?;
    info!("[Publisher] Subscriber connected");

    // CRITICAL: Wait for subscriber to complete its handshake AND send subscription
    // The accept_subscriber() only completes the publisher's side of handshake.
    // The subscriber still needs time to:
    // 1. Complete its side of the handshake
    // 2. Send subscription message
    // 3. Be ready to receive messages
    //
    // TODO: Implement proper ready signaling (e.g., wait for subscription message)
    std::thread::sleep(Duration::from_millis(2000));

    info!("[Publisher] Publishing events immediately...");

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
        // Note: No delay - messages sent as fast as possible
    }

    info!("[Publisher] Done publishing");

    // Wait for subscriber to finish receiving
    subscriber_handle.await;

    Ok(())
}



async fn run_subscriber(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    info!("[Subscriber] Connecting to port {}...", port);
    let mut socket = SubSocket::connect(&format!("127.0.0.1:{}", port)).await?;
    info!("[Subscriber] Connected!");

    // Subscribe to trade events only
    info!("[Subscriber] Subscribing to 'trade.' prefix");
    socket.subscribe(b"trade.").await?;
    info!("[Subscriber] Subscribed!");

    info!("[Subscriber] Waiting for events...\n");

    // Receive 4 trade events (we're publishing 6 total, 4 are trade.*)
    for i in 0..4 {
        match socket.recv().await {
            Ok(Some(message)) => {
                if message.len() >= 2 {
                    let topic = std::str::from_utf8(&message[0]).unwrap_or("<invalid>");
                    let data = std::str::from_utf8(&message[1]).unwrap_or("<invalid>");
                    info!("[Subscriber] Message {}: {topic} -> {data}", i + 1);
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
