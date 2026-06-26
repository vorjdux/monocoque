/// `PubSub` Events Example
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
use std::sync::mpsc;
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
    let mut pub_socket = PubSocket::bind(format!("127.0.0.1:{port}")).await?;
    info!("[Publisher] Bound to port {}", port);

    // Channel to signal when subscriber has finished subscribing and is ready
    let (ready_tx, ready_rx) = mpsc::channel::<()>();

    // Start subscriber in background FIRST (before accept)
    let subscriber_handle = compio::runtime::spawn(async move {
        if let Err(e) = run_subscriber(port, ready_tx).await {
            error!("[Subscriber] Error: {e}");
        }
    });

    // Accept subscriber connection (yields to allow subscriber task to connect)
    pub_socket.accept_subscriber().await?;
    info!("[Publisher] Subscriber connected");

    // Wait for subscriber to finish subscribing via channel signal.
    // We poll non-blocking with short async sleeps so the runtime stays live.
    // This replaces the old hardcoded 2000ms sleep.
    while ready_rx.try_recv() == Err(mpsc::TryRecvError::Empty) {
        compio::time::sleep(Duration::from_millis(10)).await;
    }

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
    }

    info!("[Publisher] Done publishing");

    // Wait for subscriber to finish receiving
    subscriber_handle.await;

    Ok(())
}

#[allow(clippy::future_not_send)]
async fn run_subscriber(
    port: u16,
    ready_tx: mpsc::Sender<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("[Subscriber] Connecting to port {}...", port);
    let mut socket = SubSocket::connect(&format!("127.0.0.1:{port}")).await?;
    info!("[Subscriber] Connected!");

    // Subscribe to trade events only
    info!("[Subscriber] Subscribing to 'trade.' prefix");
    socket.subscribe(b"trade.").await?;
    info!("[Subscriber] Subscribed!");

    // Signal publisher that subscription is active
    let _ = ready_tx.send(());

    info!("[Subscriber] Waiting for events...\n");

    // Receive 4 trade events (we publish 6 total, 4 are trade.*)
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
