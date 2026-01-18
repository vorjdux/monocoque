/// Multi-subscriber PUB/SUB example demonstrating:
/// - Multiple SUB sockets connecting to a single PUB socket
/// - Topic-based filtering per subscriber
/// - High-performance broadcast with minimal overhead

use bytes::Bytes;
use monocoque::zmq::prelude::*;
use std::time::Duration;
use compio::runtime::Runtime;
use tracing::{error, info};
use tracing_subscriber::FmtSubscriber;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Runtime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("=== Multi-Subscriber PUB/SUB Test ===");

    // Create publisher on random port
    let mut pub_socket = PubSocket::bind("127.0.0.1:0").await?;
    let actual_port = pub_socket.local_addr()?.port();
    info!("Publisher bound to port {}", actual_port);

    // Spawn 3 subscribers with different topic subscriptions
    let port1 = actual_port;
    let sub1_handle = compio::runtime::spawn(async move {
        if let Err(e) = run_subscriber(port1, "weather", 1).await {
            error!("Subscriber 1 failed: {}", e);
        }
    });

    let port2 = actual_port;
    let sub2_handle = compio::runtime::spawn(async move {
        if let Err(e) = run_subscriber(port2, "news", 2).await {
            error!("Subscriber 2 failed: {}", e);
        }
    });

    let port3 = actual_port;
    let sub3_handle = compio::runtime::spawn(async move {
        if let Err(e) = run_subscriber(port3, "", 3).await {
            error!("Subscriber 3 (all topics) failed: {}", e);
        }
    });

    // Wait for subscribers to connect
    info!("[PUB] Waiting for subscribers to connect...");
    for i in 1..=3 {
        let id = pub_socket.accept_subscriber().await?;
        info!("[PUB] Subscriber {} connected (id={})", i, id);
    }

    // Give subscribers time to send subscriptions
    std::thread::sleep(Duration::from_millis(100));

    info!("[PUB] Broadcasting to {} subscribers", pub_socket.subscriber_count());

    // Publish messages on different topics
    let topics = vec![
        ("weather.temp", "22C in New York"),
        ("weather.wind", "15 km/h from NE"),
        ("news.tech", "Rust 1.75 released!"),
        ("news.world", "Peace talks begin"),
        ("sports.soccer", "Final score: 3-2"),
        ("weather.rain", "20% chance of rain"),
        ("news.local", "New park opening"),
    ];

    for (topic, msg) in topics {
        info!("[PUB] Publishing: {} -> {}", topic, msg);
        
        // Send as multipart: [topic, message]
        pub_socket.send(vec![
            Bytes::from(topic.as_bytes()),
            Bytes::from(msg.as_bytes()),
        ]).await?;
        
        std::thread::sleep(Duration::from_millis(100));
    }

    info!("[PUB] Publisher finished");
    
    // Give subscribers time to receive all messages
    std::thread::sleep(Duration::from_secs(1));

    // Wait for all subscribers to complete
    let _ = futures::join!(sub1_handle, sub2_handle, sub3_handle);

    info!("=== Test Complete ===");
    Ok(())
}

async fn run_subscriber(port: u16, topic: &str, id: u8) -> std::io::Result<()> {
    info!("[SUB{}] Starting subscriber for topic: '{}'", id, topic);
    
    // Connect to publisher
    let endpoint = format!("127.0.0.1:{}", port);
    let mut sub_socket = SubSocket::connect(&endpoint).await?;
    info!("[SUB{}] Connected to {}", id, endpoint);

    // Subscribe to topic
    sub_socket.subscribe(topic.as_bytes()).await?;
    info!("[SUB{}] Subscribed to '{}'", id, topic);

    // Give publisher time to process subscription
    std::thread::sleep(Duration::from_millis(50));

    // Receive messages
    let mut msg_count = 0;
    let start = std::time::Instant::now();
    
    while start.elapsed() < Duration::from_secs(3) {
        match sub_socket.recv().await? {
            Some(frames) if frames.len() >= 2 => {
                let topic_frame = String::from_utf8_lossy(&frames[0]);
                let msg_frame = String::from_utf8_lossy(&frames[1]);
                info!("[SUB{}] Received: {} -> {}", id, topic_frame, msg_frame);
                msg_count += 1;
            }
            Some(_) => {
                info!("[SUB{}] Received incomplete message", id);
            }
            None => {
                // No message available, brief sleep
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    info!("[SUB{}] Subscriber finished. Received {} messages", id, msg_count);
    Ok(())
}
