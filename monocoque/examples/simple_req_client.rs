//! Simple REQ Client for Interop Testing
//!
//! This is a standalone example using monocoque-zmtp directly for testing.

use bytes::Bytes;
use compio::net::TcpStream;
use std::env;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse port from args (default 5555)
    let args: Vec<String> = env::args().collect();
    let port = if args.len() > 2 && args[1] == "--port" {
        args[2].parse::<u16>()?
    } else {
        5555
    };

    // Connect to server
    let addr = format!("127.0.0.1:{}", port);
    let stream = TcpStream::connect(&addr).await?;
    println!("Connected to {}", addr);

    let mut socket = monocoque_zmtp::ReqSocket::new(stream).await?;

    // Send request
    let msg = vec![Bytes::from("Hello from Monocoque")];
    socket.send(msg).await?;
    println!("Sent: Hello from Monocoque");

    // Receive reply
    if let Some(reply) = socket.recv().await? {
        println!("Received: {}", String::from_utf8_lossy(&reply[0]));
    }

    Ok(())
}
