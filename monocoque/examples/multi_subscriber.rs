/// Multi-subscriber PUB/SUB example demonstrating:
/// - Multiple SUB sockets connecting to a single PUB socket
/// - Topic-based filtering per subscriber
/// - High-performance broadcast with minimal overhead

use monocoque::prelude::*;
use monocoque::ZmqSocket;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tracing::{debug, error, info};
use tracing_subscriber::FmtSubscriber;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("=== Multi-Subscriber PUB/SUB Test ===");

    // Use Arc<Mutex> to share the port between publisher and subscribers
    let port = Arc::new(Mutex::new(None));

    // Spawn publisher
    let pub_port = port.clone();
    let publisher_handle = tokio::spawn(async move {
        if let Err(e) = run_publisher(pub_port).await {
            error!("Publisher failed: {}", e);
        }
    });

    // Wait for publisher to bind and share the port
    sleep(Duration::from_millis(100)).await;

    // Get the actual port
    let actual_port = {
        let port_guard = port.lock().await;
        port_guard.expect("Publisher should have set the port")
    };
    info!("Publisher bound to port {}", actual_port);

    // Spawn 3 subscribers with different topic subscriptions
    let sub1_handle = tokio::spawn(async move {
        if let Err(e) = run_subscriber(actual_port, "weather", 1).await {
            error!("Subscriber 1 failed: {}", e);
        }
    });

    let sub2_handle = tokio::spawn(async move {
        if let Err(e) = run_subscriber(actual_port, "news", 2).await {
            error!("Subscriber 2 failed: {}", e);
        }
    });

    let sub3_handle = tokio::spawn(async move {
        if let Err(e) = run_subscriber(actual_port, "", 3).await {
            error!("Subscriber 3 (all topics) failed: {}", e);
        }
    });

    // Let them run for a bit
    sleep(Duration::from_secs(5)).await;

    info!("Waiting for all tasks to complete...");
    let _ = tokio::join!(publisher_handle, sub1_handle, sub2_handle, sub3_handle);

    info!("=== Test Complete ===");
    Ok(())
}

async fn run_publisher(port: Arc<Mutex<Option<u16>>>) -> std::io::Result<()> {
    info!("[PUB] Creating publisher socket");

    // Bind to a random port
    let mut pub_socket = ZmqSocket::bind("tcp://127.0.0.1:0", SocketType::Pub).await?;

    // Get the actual port
    let actual_port = pub_socket.local_addr()?.port();
    info!("[PUB] Bound to port {}", actual_port);

    // Share the port with subscribers
    {
        let mut port_guard = port.lock().await;
        *port_guard = Some(actual_port);
    }

    // Accept multiple subscribers (in real implementation this would be in a loop)
    info!("[PUB] Accepting subscriber 1...");
    sleep(Duration::from_millis(200)).await;
    // Note: accept_subscriber would be called in actual implementation

    info!("[PUB] Accepting subscriber 2...");
    sleep(Duration::from_millis(200)).await;

    info!("[PUB] Accepting subscriber 3...");
    sleep(Duration::from_millis(200)).await;

    // Process subscriptions from all subscribers
    info!("[PUB] Processing subscriptions...");
    pub_socket.process_subscriptions().await?;
    sleep(Duration::from_millis(200)).await;
    pub_socket.process_subscriptions().await?;

    // Publish messages on different topics
    let topics = vec![
        ("weather.temp", "22°C in New York"),
        ("weather.wind", "15 km/h from NE"),
        ("news.tech", "Rust 1.75 released!"),
        ("news.world", "Peace talks begin"),
        ("sports.soccer", "Final score: 3-2"),
        ("weather.rain", "20% chance of rain"),
        ("news.local", "New park opening"),
    ];

    for (topic, msg) in topics {
        info!("[PUB] Publishing: {} -> {}", topic, msg);
        
        // Process subscriptions before each send to ensure all subscribers are ready
        pub_socket.process_subscriptions().await?;
        
        // Send topic and message as separate frames
        pub_socket.send(topic.as_bytes()).await?;
        pub_socket.send(msg.as_bytes()).await?;
        
        sleep(Duration::from_millis(200)).await;
    }

    // Process any final subscription updates
    pub_socket.process_subscriptions().await?;
    info!("[PUB] Publisher finished");
    
    // Keep running a bit longer to let subscribers finish
    sleep(Duration::from_secs(1)).await;
    
    Ok(())
}

async fn run_subscriber(port: u16, topic: &str, id: u8) -> std::io::Result<()> {
    info!("[SUB{}] Starting subscriber for topic: '{}'", id, topic);
    
    // Connect to publisher
    let endpoint = format!("tcp://127.0.0.1:{}", port);
    let mut sub_socket = ZmqSocket::connect(&endpoint, SocketType::Sub).await?;
    info!("[SUB{}] Connected to {}", id, endpoint);

    // Subscribe to topic
    sub_socket.subscribe(topic.as_bytes()).await?;
    info!("[SUB{}] Subscribed to '{}'", id, topic);

    // Give publisher time to process subscription
    sleep(Duration::from_millis(300)).await;

    // Receive messages
    let mut msg_count = 0;
    loop {
        // Use timeout to avoid blocking forever
        match timeout(Duration::from_secs(2), sub_socket.recv()).await {
            Ok(Ok(msg)) => {
                let topic_str = String::from_utf8_lossy(&msg);
                
                // Receive the actual message (second frame)
                match timeout(Duration::from_millis(500), sub_socket.recv()).await {
                    Ok(Ok(data)) => {
                        let msg_str = String::from_utf8_lossy(&data);
                        info!("[SUB{}] Received: {} -> {}", id, topic_str, msg_str);
                        msg_count += 1;
                    }
                    Ok(Err(e)) => {
                        error!("[SUB{}] Error receiving message data: {}", id, e);
                        break;
                    }
                    Err(_) => {
                        debug!("[SUB{}] Timeout waiting for message data", id);
                    }
                }
            }
            Ok(Err(e)) => {
                error!("[SUB{}] Error receiving topic: {}", id, e);
                break;
            }
            Err(_) => {
                debug!("[SUB{}] Timeout - no more messages", id);
                break;
            }
        }
    }

    info!("[SUB{}] Subscriber finished. Received {} messages", id, msg_count);
    Ok(())
}
