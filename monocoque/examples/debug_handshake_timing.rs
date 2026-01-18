/// Debug example to measure handshake timing
use monocoque::zmq::{PubSocket, SubSocket};
use std::time::Instant;
use tracing::info;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let port = portpicker::pick_unused_port().expect("No ports available");
    info!("Using port {}", port);

    // Start publisher
    let start = Instant::now();
    let mut pub_socket = PubSocket::bind(format!("127.0.0.1:{}", port)).await?;
    info!("Publisher bound in {:?}", start.elapsed());

    // Start subscriber in background
    let sub_handle = compio::runtime::spawn(async move {
        let start = Instant::now();
        info!("[SUB] Starting connect...");
        
        let before_connect = Instant::now();
        let socket = SubSocket::connect(&format!("127.0.0.1:{}", port)).await;
        info!("[SUB] connect() completed in {:?}", before_connect.elapsed());
        
        info!("[SUB] Total time: {:?}", start.elapsed());
        socket
    });

    // Small delay
    compio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Accept subscriber
    let accept_start = Instant::now();
    info!("[PUB] Accepting subscriber...");
    pub_socket.accept_subscriber().await?;
    info!("[PUB] accept_subscriber() completed in {:?}", accept_start.elapsed());

    // Wait for subscriber
    let result = sub_handle.await;
    match result {
        Ok(_) => info!("Subscriber connected successfully"),
        Err(e) => info!("Subscriber error: {}", e),
    }

    Ok(())
}
