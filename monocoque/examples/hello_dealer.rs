//! Hello DEALER Example
//!
//! This example demonstrates basic DEALER socket usage:
//! - Connect to a server
//! - Send a simple message
//! - Receive a response
//!
//! It needs a peer: start a ROUTER that replies on tcp://127.0.0.1:5555
//! first, then run `cargo run --example hello_dealer --features zmq`.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::DealerSocket;
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    info!("Connecting to tcp://127.0.0.1:5555...");

    // Create DEALER socket and start connecting. Dialing happens in the
    // background, so this returns even if the server is not up yet.
    let mut socket = DealerSocket::new();
    socket.connect("tcp://127.0.0.1:5555").await?;

    info!("Sending message...");

    // Send a simple message; it waits for the connection
    socket.send("Hello from Monocoque!").await?;

    info!("Message sent. Waiting for response...");

    // Receive response
    let response = socket.recv().await?;

    info!("Received response: {} frames", response.len());
    for (i, frame) in response.frames().enumerate() {
        info!("  Frame {}: {} bytes", i, frame.len());
        if let Ok(s) = std::str::from_utf8(frame) {
            info!("    Content: {s}");
        }
    }

    Ok(())
}
