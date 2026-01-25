//! Minimal REP server with detailed logging for debugging

use bytes::Bytes;
use compio::net::TcpListener;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable debug logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(true)
        .init();

    println!("Starting REP server on 127.0.0.1:15555...");
    
    let listener = TcpListener::bind("127.0.0.1:15555").await?;
    println!("✓ Listening on 127.0.0.1:15555");

    println!("Waiting for connection...");
    let (stream, peer) = listener.accept().await?;
    println!("✓ Accepted connection from {}", peer);

    println!("Creating RepSocket (will perform handshake)...");
    let mut socket = monocoque_zmtp::RepSocket::new(stream).await?;
    println!("✓ RepSocket created, handshake complete");

    println!("Waiting for request...");
    match socket.recv().await? {
        Some(msg) => {
            println!("✓ Received {} frames", msg.len());
            for (i, frame) in msg.iter().enumerate() {
                println!("  Frame {}: {} bytes: {}", i, frame.len(), 
                    String::from_utf8_lossy(frame));
            }
            
            // Echo back
            let reply = vec![Bytes::from(format!("Echo: {}", 
                String::from_utf8_lossy(&msg[0])))];
            
            println!("Sending reply...");
            socket.send(reply).await?;
            println!("✓ Reply sent");
        }
        None => {
            println!("✗ Connection closed");
        }
    }

    println!("✓ Done");
    Ok(())
}
