/// Minimal multi-subscriber PUB test with worker pool
use bytes::Bytes;
use monocoque::zmq::PubSocket;
use std::thread;
use std::time::Duration;
use tracing::info;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Starting multi-subscriber publisher...");
    
    let mut pub_socket = PubSocket::bind("127.0.0.1:5556").await?;
    info!("Publisher bound to 127.0.0.1:5556");
    info!("Worker pool size: {} workers", num_cpus::get());
    
    // Wait for subscribers to connect
    info!("Waiting 2 seconds for subscribers to connect...");
    thread::sleep(Duration::from_secs(2));
    
    // Send test messages
    for i in 0..10 {
        let msg = vec![Bytes::from(format!("test.{}", i)), Bytes::from("data")];
        pub_socket.send(msg).await?;
        info!("Sent message {}", i);
        thread::sleep(Duration::from_millis(100));
    }
    
    info!("Publisher done - sent 10 messages");
    Ok(())
}
