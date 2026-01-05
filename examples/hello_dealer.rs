/// Hello DEALER Example
///
/// This example demonstrates basic DEALER socket usage:
/// - Connect to a server
/// - Send a simple message
/// - Receive a response
///
/// Run this after starting a ZMQ ROUTER server on port 5555

use bytes::Bytes;
use monocoque_zmtp::DealerSocket;
use compio::net::TcpStream;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to tcp://127.0.0.1:5555...");
    
    // Connect to server
    let stream = TcpStream::connect("127.0.0.1:5555").await?;
    
    // Create DEALER socket
    let socket = DealerSocket::new(stream);
    
    println!("Connected! Sending message...");
    
    // Send a simple message
    socket.send(vec![Bytes::from("Hello from Monocoque!")]).await?;
    
    println!("Message sent. Waiting for response...");
    
    // Receive response
    let response = socket.recv().await?;
    
    println!("Received response: {} frames", response.len());
    for (i, frame) in response.iter().enumerate() {
        println!("  Frame {}: {} bytes", i, frame.len());
        if let Ok(s) = std::str::from_utf8(frame) {
            println!("    Content: {}", s);
        }
    }
    
    Ok(())
}
