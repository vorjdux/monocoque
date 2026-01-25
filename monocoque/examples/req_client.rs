//! REQ Client for Interop Testing

use monocoque_zmtp::req::ReqSocket;
use bytes::Bytes;
use std::env;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let port = if args.len() > 2 && args[1] == "--port" {
        args[2].parse::<u16>()?
    } else {
        5555
    };

    let addr = format!("127.0.0.1:{}", port);
    let stream = compio::net::TcpStream::connect(&addr).await?;
    let mut socket = ReqSocket::new(stream).await?;

    // Send request
    socket.send(vec![Bytes::from("Hello from Monocoque")]).await?;
    
    // Receive reply
    if let Some(reply) = socket.recv().await? {
        println!("Reply: {}", String::from_utf8_lossy(&reply[0]));
    }

    Ok(())
}
