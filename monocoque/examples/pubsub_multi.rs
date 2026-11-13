//! Multi-subscriber PUB/SUB example.
//!
//! This example demonstrates a single PUB socket broadcasting to multiple
//! SUB sockets with different topic subscriptions.
//!
//! Architecture:
//! - 1 PUB socket: Broadcasts events with different topic prefixes
//! - 3 SUB sockets: Each subscribes to different topics
//!   - SUB1: Subscribes to "news.tech"
//!   - SUB2: Subscribes to "news.finance"
//!   - SUB3: Subscribes to all news ("news")
//!
//! Expected behavior:
//! - SUB1 receives only tech news
//! - SUB2 receives only finance news
//! - SUB3 receives all news (tech + finance)

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::prelude::*;
use std::time::Duration;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const ROUNDS: usize = 5;

#[allow(clippy::future_not_send)]
async fn publisher_task(mut pub_socket: PubSocket, subscribers: usize) -> Result<()> {
    // Wait until every subscriber has connected and its subscription has
    // arrived; anything published before that would be dropped.
    info!("[PUB] Waiting for subscribers...");
    loop {
        let ready = pub_socket
            .peer_ids()
            .into_iter()
            .filter(|&peer| pub_socket.subscriber_prefixes(peer) > 0)
            .count();
        if ready == subscribers {
            break;
        }
        rt::sleep(Duration::from_millis(10)).await;
    }

    info!(
        "[PUB] Ready to broadcast to {} subscribers",
        pub_socket.peer_count()
    );

    // Send messages with different topics
    info!("[PUB] Broadcasting messages...");

    for i in 0..ROUNDS {
        // Tech news
        let n = pub_socket
            .send(Msg::from([
                Bytes::from("news.tech"),
                Bytes::from(format!("Tech update #{i}")),
            ]))
            .await?;
        info!("[PUB] Sent tech news #{} to {} subscribers", i, n);

        // Finance news
        let n = pub_socket
            .send(Msg::from([
                Bytes::from("news.finance"),
                Bytes::from(format!("Finance update #{i}")),
            ]))
            .await?;
        info!("[PUB] Sent finance news #{} to {} subscribers", i, n);
    }

    info!("[PUB] All messages sent");
    pub_socket.close().await?;
    info!("[PUB] Publisher complete");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn subscriber_task(
    name: &str,
    endpoint: &str,
    topic: &str,
    expected_count: usize,
) -> Result<()> {
    info!("[{}] Connecting to {}", name, endpoint);

    let mut sub_socket = SubSocket::new();
    sub_socket.connect(endpoint).await?;
    sub_socket.subscribe(topic.as_bytes()).await?;
    info!("[{}] Subscribed to '{}'", name, topic);

    let mut received = 0;
    while received < expected_count {
        let msg = sub_socket.recv().await?;
        let topic = String::from_utf8_lossy(msg.get(0).unwrap_or_default());
        let content = String::from_utf8_lossy(msg.get(1).unwrap_or_default());
        info!(
            "[{}] Received: topic='{}' content='{}'",
            name, topic, content
        );
        received += 1;
    }

    info!(
        "[{}] Successfully received all {} messages",
        name, expected_count
    );
    Ok(())
}

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Bind the publisher first on an ephemeral port
    let mut pub_socket = PubSocket::new();
    let endpoint = pub_socket.bind("tcp://127.0.0.1:0").await?;
    info!("[PUB] Bound to {}", endpoint);

    let pub_handle = rt::spawn(async move { publisher_task(pub_socket, 3).await });

    let subs = [
        ("SUB1", "news.tech", ROUNDS),
        ("SUB2", "news.finance", ROUNDS),
        ("SUB3", "news", ROUNDS * 2),
    ];
    let handles: Vec<_> = subs
        .into_iter()
        .map(|(name, topic, count)| {
            let endpoint = endpoint.clone();
            let handle =
                rt::spawn(async move { subscriber_task(name, &endpoint, topic, count).await });
            (name, handle)
        })
        .collect();

    // Wait for publisher to complete
    let pub_result = rt::join(pub_handle).await;

    // Report results
    info!("=== Results ===");
    match pub_result {
        Ok(()) => info!("Publisher: success"),
        Err(e) => error!("Publisher: error: {:?}", e),
    }
    for (name, handle) in handles {
        match rt::join(handle).await {
            Ok(()) => info!("{name}: success"),
            Err(e) => error!("{name}: error: {:?}", e),
        }
    }

    Ok(())
}
