//! `PubSub` Events Example
//!
//! This example demonstrates PUB/SUB pattern for event distribution:
//! - Publisher broadcasts events on different topics
//! - Subscriber filters events by topic prefix
//!
//! Architecture:
//! - PUB socket broadcasts to all subscribers
//! - SUB socket subscribes to specific topics
//! - Topics are prefix-matched (e.g., "trade." matches "trade.BTC", "trade.ETH")

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{Msg, PubSocket, Result, SubSocket};
use std::time::Duration;
use tracing::{error, info};

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== PubSub Events Example ===\n");

    // Start publisher on an ephemeral port
    let mut pub_socket = PubSocket::new();
    let endpoint = pub_socket.bind("tcp://127.0.0.1:0").await?;
    info!("[Publisher] Bound to {}", endpoint);

    // Start subscriber in background
    let subscriber_handle = rt::spawn(async move {
        if let Err(e) = run_subscriber(&endpoint).await {
            error!("[Subscriber] Error: {e}");
        }
    });

    // A PUB drops messages nobody has subscribed to yet, so wait until the
    // subscriber's prefix has reached the publisher.
    while !pub_socket
        .peer_ids()
        .into_iter()
        .any(|peer| pub_socket.subscriber_prefixes(peer) > 0)
    {
        rt::sleep(Duration::from_millis(10)).await;
    }

    info!("[Publisher] Publishing events...");

    // Publish events on different topics
    let events = [
        ("trade.BTC", "BTC/USD: 45000"),
        ("trade.ETH", "ETH/USD: 3000"),
        ("news.crypto", "New regulation announced"),
        ("trade.BTC", "BTC/USD: 45100"),
        ("alert.system", "System maintenance in 1 hour"),
        ("trade.ETH", "ETH/USD: 3050"),
    ];

    for (topic, data) in events {
        let message = Msg::from([Bytes::from(topic), Bytes::from(data)]);
        let delivered = pub_socket.send(message).await?;
        info!("[Publisher] Publishing: {topic} -> {data} ({delivered} subscribers)");
    }

    info!("[Publisher] Done publishing");

    // Wait for subscriber to finish receiving
    rt::join(subscriber_handle).await;

    Ok(())
}

#[allow(clippy::future_not_send)]
async fn run_subscriber(endpoint: &str) -> Result<()> {
    info!("[Subscriber] Connecting to {}...", endpoint);
    let mut socket = SubSocket::new();
    socket.connect(endpoint).await?;

    // Subscribe to trade events only
    info!("[Subscriber] Subscribing to 'trade.' prefix");
    socket.subscribe(b"trade.").await?;

    info!("[Subscriber] Waiting for events...\n");

    // Receive 4 trade events (we publish 6 total, 4 are trade.*)
    for i in 0..4 {
        let message = socket.recv().await?;
        if let (Some(topic), Some(data)) = (message.get(0), message.get(1)) {
            let topic = std::str::from_utf8(topic).unwrap_or("<invalid>");
            let data = std::str::from_utf8(data).unwrap_or("<invalid>");
            info!("[Subscriber] Message {}: {topic} -> {data}", i + 1);
        }
    }

    info!("[Subscriber] Done receiving");
    Ok(())
}
