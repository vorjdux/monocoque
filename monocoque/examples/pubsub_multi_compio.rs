/// Multi-subscriber PUB/SUB example.
///
/// This example demonstrates a single PUB socket broadcasting to multiple
/// SUB sockets with different topic subscriptions.
///
/// Architecture:
/// - 1 PUB socket: Broadcasts events with different topic prefixes
/// - 3 SUB sockets: Each subscribes to different topics
///   - SUB1: Subscribes to "news.tech"
///   - SUB2: Subscribes to "news.finance"  
///   - SUB3: Subscribes to all news ("news")
///
/// Expected behavior:
/// - SUB1 receives only tech news
/// - SUB2 receives only finance news
/// - SUB3 receives all news (tech + finance)

use bytes::Bytes;
use compio::runtime::Runtime;
use monocoque::zmq::prelude::*;
use std::time::Duration;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

async fn publisher_task(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    info!("[PUB] Starting publisher on port {}", port);
    
    let mut pub_socket = PubSocket::bind(format!("127.0.0.1:{}", port)).await?;
    
    // Wait for all 3 subscribers to connect
    info!("[PUB] Waiting for subscribers...");
    for i in 1..=3 {
        let id = pub_socket.accept_subscriber().await?;
        info!("[PUB] Subscriber {} connected (id={})", i, id);
    }
    
    // Give subscribers time to be ready
    compio::time::sleep(Duration::from_millis(200)).await;
    
    info!("[PUB] Ready to broadcast to {} subscribers", pub_socket.subscriber_count());
    
    // Send messages with different topics
    info!("[PUB] Broadcasting messages...");
    
    for i in 0..5 {
        // Tech news
        pub_socket.send(vec![
            Bytes::from(format!("news.tech")),
            Bytes::from(format!("Tech update #{}", i)),
        ]).await?;
        info!("[PUB] Sent tech news #{}", i);
        
        // Finance news
        pub_socket.send(vec![
            Bytes::from(format!("news.finance")),
            Bytes::from(format!("Finance update #{}", i)),
        ]).await?;
        info!("[PUB] Sent finance news #{}", i);
        
        // Short delay between messages
        compio::time::sleep(Duration::from_millis(50)).await;
    }
    
    info!("[PUB] All messages sent, waiting for subscribers to finish...");
    compio::time::sleep(Duration::from_millis(500)).await;
    
    info!("[PUB] Publisher complete");
    Ok(())
}

async fn subscriber_task(
    name: &str,
    port: u16,
    topic: &str,
    expected_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("[{}] Connecting to publisher on port {}", name, port);
    
    // Connect and subscribe
    let mut sub_socket = SubSocket::connect(&format!("127.0.0.1:{}", port)).await?;
    sub_socket.subscribe(topic.as_bytes()).await?;
    info!("[{}] Subscribed to '{}'", name, topic);
    
    // Receive messages
    let mut received = 0;
    while received < expected_count {
        match compio::time::timeout(Duration::from_secs(2), sub_socket.recv()).await {
            Ok(Ok(Some(msg))) => {
                let topic = String::from_utf8_lossy(&msg[0]);
                let content = String::from_utf8_lossy(&msg[1]);
                info!("[{}] Received: topic='{}' content='{}'", name, topic, content);
                received += 1;
            }
            Ok(Ok(None)) => {
                error!("[{}] Connection closed", name);
                break;
            }
            Ok(Err(e)) => {
                error!("[{}] Receive error: {}", name, e);
                break;
            }
            Err(_) => {
                error!("[{}] Timeout waiting for message", name);
                break;
            }
        }
    }
    
    if received == expected_count {
        info!("[{}] ✓ Successfully received all {} messages", name, expected_count);
    } else {
        error!("[{}] ✗ Expected {} messages, received {}", name, expected_count, received);
    }
    
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"))
        )
        .init();

    // Use a random available port to avoid conflicts
    let port = portpicker::pick_unused_port().expect("No ports available");
    info!("Using port {}", port);

    let runtime = Runtime::new()?;
    
    runtime.block_on(async {
        // Start publisher first to bind the port
        let pub_handle = compio::runtime::spawn({
            let port = port;
            async move {
                publisher_task(port).await
            }
        });
        
        // Give publisher time to bind
        compio::time::sleep(Duration::from_millis(200)).await;
        
        // Now spawn subscriber tasks
        let sub1_handle = compio::runtime::spawn({
            let port = port;
            async move {
                subscriber_task("SUB1", port, "news.tech", 5).await
            }
        });
        
        let sub2_handle = compio::runtime::spawn({
            let port = port;
            async move {
                subscriber_task("SUB2", port, "news.finance", 5).await
            }
        });
        
        let sub3_handle = compio::runtime::spawn({
            let port = port;
            async move {
                subscriber_task("SUB3", port, "news", 10).await // Receives both tech and finance
            }
        });
        
        // Wait for all tasks to complete
        let pub_result = pub_handle.await;
        let sub1_result = sub1_handle.await;
        let sub2_result = sub2_handle.await;
        let sub3_result = sub3_handle.await;
        
        // Report results
        info!("=== Results ===");
        match pub_result {
            Ok(_) => info!("Publisher: ✓ Success"),
            Err(e) => error!("Publisher: ✗ Error: {:?}", e),
        }
        match sub1_result {
            Ok(_) => info!("SUB1 (tech): ✓ Success"),
            Err(e) => error!("SUB1 (tech): ✗ Error: {:?}", e),
        }
        match sub2_result {
            Ok(_) => info!("SUB2 (finance): ✓ Success"),
            Err(e) => error!("SUB2 (finance): ✗ Error: {:?}", e),
        }
        match sub3_result {
            Ok(_) => info!("SUB3 (all news): ✓ Success"),
            Err(e) => error!("SUB3 (all news): ✗ Error: {:?}", e),
        }
        
        Ok(())
    })
}
