//! Simple REP Server for Interop Testing
//!
//! This is a standalone example using monocoque-zmtp directly for testing.

use bytes::Bytes;
use compio::net::TcpListener;
use std::env;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable debug logging.
    // SAFETY: set as the very first thing in `main`, before any other thread is
    // spawned, so there is no concurrent access to the process environment.
    unsafe {
        std::env::set_var("RUST_LOG", "debug");
    }
    tracing_subscriber::fmt::init();
    // Parse port from args (default 5555)
    let args: Vec<String> = env::args().collect();
    let port = if args.len() > 2 && args[1] == "--port" {
        args[2].parse::<u16>()?
    } else {
        5555
    };

    // Bind TCP listener
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).await?;
    println!("REP server listening on {addr}");

    // Accept connection
    let (stream, peer) = listener.accept().await?;
    println!("Accepted connection from {peer}");

    let mut socket = monocoque_zmtp::RepSocket::new(stream).await?;

    // Echo loop
    loop {
        if let Some(msg) = socket.recv().await? {
            println!("Received {} frames", msg.len());

            // Echo back with "Echo: " prefix on first frame
            let mut reply = Vec::with_capacity(msg.len());

            if msg.is_empty() {
                reply.push(Bytes::from("Echo: <empty>"));
            } else {
                let first_frame = format!("Echo: {}", String::from_utf8_lossy(&msg[0]));
                reply.push(Bytes::from(first_frame));

                // Copy remaining frames as-is
                for frame in &msg[1..] {
                    reply.push(frame.clone());
                }
            }

            socket.send(reply).await?;
        } else {
            println!("Connection closed");
            break;
        }
    }

    Ok(())
}
