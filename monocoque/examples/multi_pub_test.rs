/// Integrated test - publisher with 3 subscribers
use bytes::Bytes;
use monocoque::zmq::{PubSocket, SubSocket};
use std::thread;
use std::time::Duration;
use tracing::info;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    info!("=== Multi-Subscriber Worker Pool Test ===");
    
    // Start publisher
    let mut pub_socket = PubSocket::bind("127.0.0.1:5557").await?;
    info!("Publisher bound to 127.0.0.1:5557");
    info!("Worker pool: {} workers", num_cpus::get());
    
    // Spawn 3 subscriber tasks
    for i in 1..=3 {
        compio::runtime::spawn(async move {
            // Connect subscriber
            let mut sub = SubSocket::connect("127.0.0.1:5557").await.unwrap();
            sub.subscribe(b"test").await.unwrap();
            info!("[SUB-{}] Connected and subscribed", i);
            
            // Receive messages
            let mut count = 0;
            while let Ok(Some(frames)) = sub.recv().await {
                count += 1;
                let topic = String::from_utf8_lossy(&frames[0]);
                info!("[SUB-{}] Received #{}: {}", i, count, topic);
                if count >= 5 {
                    break;
                }
            }
            info!("[SUB-{}] Completed (received {} messages)", i, count);
        }).detach();
    }
    
    // Wait for subscribers to connect and subscribe
    info!("Waiting for subscribers to connect...");
    thread::sleep(Duration::from_millis(500));
    
    // Accept 3 subscribers
    for i in 1..=3 {
        pub_socket.accept_subscriber().await?;
        info!("Accepted subscriber {}", i);
    }
    
    info!("Subscriber count: {}", pub_socket.subscriber_count());
    
    // Give subscribers time to finish subscription
    thread::sleep(Duration::from_millis(200));
    
    // Send 5 messages
    info!("Sending 5 messages...");
    for i in 0..5 {
        let msg = vec![
            Bytes::from(format!("test.msg.{}", i)),
            Bytes::from(format!("data-{}", i))
        ];
        pub_socket.send(msg).await?;
        info!("Sent message {}", i);
        thread::sleep(Duration::from_millis(50));
    }
    
    info!("All messages sent");
    
    // Wait for subscribers to receive
    thread::sleep(Duration::from_millis(500));
    
    info!("Test complete!");
    Ok(())
}
