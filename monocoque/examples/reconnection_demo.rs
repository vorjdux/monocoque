//! Example demonstrating reconnection with exponential backoff.
//!
//! This example shows how to implement a robust client that automatically
//! reconnects when the connection is lost, using exponential backoff to
//! avoid overwhelming the server.
//!
//! Run this example:
//! ```bash
//! cargo run --example reconnection_demo --features zmq
//! ```

use bytes::Bytes;
use monocoque::zmq::ReqSocket;
use monocoque::{ReconnectState, SocketOptions};
use std::io;
use std::time::Duration;
use tracing::{error, info, warn};

/// Maximum number of reconnection attempts before giving up
const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// Connect to server with automatic reconnection
async fn connect_with_retry(addr: &str, reconnect: &mut ReconnectState) -> io::Result<ReqSocket> {
    loop {
        info!(
            "Connecting to {} (attempt {})",
            addr,
            reconnect.attempt() + 1
        );

        match ReqSocket::connect(addr).await {
            Ok(socket) => {
                info!("Connected successfully");
                reconnect.reset();
                return Ok(socket);
            }
            Err(e) => {
                error!("Connection failed: {}", e);

                if reconnect.attempt() >= MAX_RECONNECT_ATTEMPTS {
                    error!("Maximum reconnection attempts reached");
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("Failed to connect after {} attempts", MAX_RECONNECT_ATTEMPTS),
                    ));
                }

                let delay = reconnect.next_delay();
                warn!(
                    "Reconnecting in {:?} (attempt {} of {})",
                    delay,
                    reconnect.attempt(),
                    MAX_RECONNECT_ATTEMPTS
                );
                compio::time::sleep(delay).await;
            }
        }
    }
}

/// Main communication loop with automatic reconnection on errors
async fn communication_loop(addr: &str) -> io::Result<()> {
    // Configure reconnection with exponential backoff
    let options = SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(100)) // Start with 100ms
        .with_reconnect_ivl_max(Duration::from_secs(30)); // Cap at 30 seconds

    let mut reconnect_state = ReconnectState::new(&options);

    loop {
        // Connect (or reconnect) to server
        let mut socket = match connect_with_retry(addr, &mut reconnect_state).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to establish connection: {}", e);
                return Err(e);
            }
        };

        // Send requests until connection fails
        let mut request_num = 0;
        loop {
            request_num += 1;
            let message = format!("Request #{}", request_num);
            
            info!("Sending: {}", message);
            let request = vec![Bytes::from(message)];

            match socket.send(request).await {
                Ok(()) => {
                    info!("Request sent successfully");
                }
                Err(e) => {
                    error!("Send failed: {}", e);
                    let delay = reconnect_state.next_delay();
                    warn!("Connection lost, reconnecting in {:?}", delay);
                    compio::time::sleep(delay).await;
                    break; // Break inner loop to reconnect
                }
            }

            // Wait for response
            match socket.recv().await {
                Some(response) => {
                    if let Some(data) = response.first() {
                        info!("Received: {}", String::from_utf8_lossy(data));
                        reconnect_state.reset(); // Reset on successful communication
                    }
                }
                None => {
                    warn!("Connection closed by server");
                    let delay = reconnect_state.next_delay();
                    warn!("Reconnecting in {:?}", delay);
                    compio::time::sleep(delay).await;
                    break; // Break inner loop to reconnect
                }
            }

            // Wait a bit between requests
            compio::time::sleep(Duration::from_secs(1)).await;
        }

        // Check if we've exceeded max reconnection attempts
        if reconnect_state.attempt() >= MAX_RECONNECT_ATTEMPTS {
            error!("Maximum reconnection attempts reached, giving up");
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Maximum reconnection attempts exceeded",
            ));
        }
    }
}

fn main() -> io::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // Run the async communication loop
    compio::runtime::Runtime::new()?.block_on(async {
        let addr = "127.0.0.1:5555";
        info!("Starting client, connecting to {}", addr);
        
        match communication_loop(addr).await {
            Ok(()) => {
                info!("Client finished successfully");
                Ok(())
            }
            Err(e) => {
                error!("Client failed: {}", e);
                Err(e)
            }
        }
    })
}
