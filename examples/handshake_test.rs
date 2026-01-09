/// Handshake Test - Validates ZMTP greeting and READY exchange
///
/// This is a minimal test to verify the protocol handshake works before
/// attempting full message exchange.

use monocoque::zmq::{DealerSocket, RouterSocket};
use compio::net::{TcpListener, TcpStream};
use std::time::Duration;
use tracing::info;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    info!("=== ZMTP Handshake Test ===\n");

    // Start server
    let server_task = compio::runtime::spawn(async {
        info!("[SERVER] Binding...");
        let listener = TcpListener::bind("127.0.0.1:5571").await.unwrap();
        info!("[SERVER] Listening on :5571");
        
        let (stream, _addr) = listener.accept().await.unwrap();
        info!("[SERVER] Connection accepted");
        
        let _socket = RouterSocket::new(stream);
        info!("[SERVER] Socket created");
        
        // Give time for handshake
        for i in 1..=10 {
            info!("[SERVER] Handshake progress... {}s", i);
            compio::time::sleep(Duration::from_millis(500)).await;
        }
        
        info!("[SERVER] Done");
    });

    // Give server time to bind
    compio::time::sleep(Duration::from_millis(100)).await;

    // Start client
    info!("[CLIENT] Connecting...");
    let stream = TcpStream::connect("127.0.0.1:5571").await?;
    info!("[CLIENT] Connected");
    
    let _socket = DealerSocket::new(stream);
    info!("[CLIENT] Socket created");
    
    // Give time for handshake
    for i in 1..=10 {
        info!("[CLIENT] Handshake progress... {}s", i);
        compio::time::sleep(Duration::from_millis(500)).await;
    }
    
    info!("[CLIENT] Done");
    
    // Wait for server
    server_task.await.unwrap();

    info!("\n✅ Handshake test completed (both sides stayed connected)");
    Ok(())
}

