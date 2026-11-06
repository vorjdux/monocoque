//! Multi-subscriber PUB/SUB example demonstrating:
//! - Multiple SUB sockets connecting to a single PUB socket
//! - Topic-based filtering per subscriber
//! - High-performance broadcast with minimal overhead
//!
//! Run with: `cargo run --example multi_subscriber --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::prelude::{Bytes, MonocoqueError, Msg, PubSocket, SocketOptions, SubSocket};
use std::time::Duration;
use tracing::{error, info};
use tracing_subscriber::FmtSubscriber;

const SUBSCRIBERS: usize = 3;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("=== Multi-Subscriber PUB/SUB Test ===");

    // Create publisher on random port
    let mut pub_socket = PubSocket::new();
    let endpoint = pub_socket.bind("tcp://127.0.0.1:0").await?;
    info!("Publisher bound to {}", endpoint);

    // Spawn 3 subscribers with different topic subscriptions
    let mut handles = Vec::new();
    for (id, topic) in [(1, "weather"), (2, "news"), (3, "")] {
        let endpoint = endpoint.clone();
        handles.push(rt::spawn(async move {
            if let Err(e) = run_subscriber(&endpoint, topic, id).await {
                error!("Subscriber {} failed: {}", id, e);
            }
        }));
    }

    // Wait until every subscriber is connected and its subscription has
    // arrived; a PUB drops whatever nobody is subscribed to yet.
    info!("[PUB] Waiting for subscribers to connect...");
    loop {
        let ids = pub_socket.peer_ids();
        if ids.len() == SUBSCRIBERS && ids.iter().all(|&id| pub_socket.subscriber_prefixes(id) > 0)
        {
            break;
        }
        rt::sleep(Duration::from_millis(10)).await;
    }

    info!(
        "[PUB] Broadcasting to {} subscribers",
        pub_socket.peer_count()
    );

    // Publish messages on different topics
    let topics = [
        ("weather.temp", "22C in New York"),
        ("weather.wind", "15 km/h from NE"),
        ("news.tech", "Rust 1.75 released!"),
        ("news.world", "Peace talks begin"),
        ("sports.soccer", "Final score: 3-2"),
        ("weather.rain", "20% chance of rain"),
        ("news.local", "New park opening"),
    ];

    for (topic, msg) in topics {
        // Send as multipart: [topic, message]
        let reached = pub_socket
            .send(Msg::from([
                Bytes::from_static(topic.as_bytes()),
                Bytes::from_static(msg.as_bytes()),
            ]))
            .await?;
        info!(
            "[PUB] Published: {} -> {} ({} subscribers)",
            topic, msg, reached
        );
    }

    info!("[PUB] Publisher finished");
    pub_socket.flush().await?;

    // Wait for all subscribers to complete
    for handle in handles {
        rt::join(handle).await;
    }

    info!("=== Test Complete ===");
    Ok(())
}

async fn run_subscriber(endpoint: &str, topic: &str, id: u8) -> monocoque::zmq::Result<()> {
    info!("[SUB{}] Starting subscriber for topic: '{}'", id, topic);

    // Stop once the publisher has gone quiet for a second
    let mut sub_socket =
        SubSocket::with_options(SocketOptions::default().with_recv_timeout(Duration::from_secs(1)));
    sub_socket.connect(endpoint).await?;
    info!("[SUB{}] Connecting to {}", id, endpoint);

    // Subscribe to topic
    sub_socket.subscribe(topic.as_bytes()).await?;
    info!("[SUB{}] Subscribed to '{}'", id, topic);

    // Receive messages
    let mut msg_count = 0;
    loop {
        match sub_socket.recv().await {
            Ok(frames) if frames.len() >= 2 => {
                let topic_frame = String::from_utf8_lossy(frames.get(0).unwrap_or_default());
                let msg_frame = String::from_utf8_lossy(frames.get(1).unwrap_or_default());
                info!("[SUB{}] Received: {} -> {}", id, topic_frame, msg_frame);
                msg_count += 1;
            }
            Ok(_) => {
                info!("[SUB{}] Received incomplete message", id);
            }
            Err(MonocoqueError::TimedOut) => break,
            Err(e) => return Err(e),
        }
    }

    info!(
        "[SUB{}] Subscriber finished. Received {} messages",
        id, msg_count
    );
    Ok(())
}
