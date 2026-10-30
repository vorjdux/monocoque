//! Basic ROUTER-DEALER Example
//!
//! Demonstrates a working request-response pattern where:
//! - ROUTER socket binds and waits for requests
//! - DEALER socket connects and sends a request
//! - ROUTER receives, processes, and replies
//! - DEALER receives the response
//!
//! This validates the complete ZMTP implementation including handshake and messaging.

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, Msg, Result, RouterSocket};
use tracing::info;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Basic ROUTER-DEALER Example ===\n");

    // Bind on an ephemeral port to avoid conflicts
    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    info!("[ROUTER] Bound to {endpoint}");

    // Start ROUTER (server) and DEALER (client)
    let router_task = rt::spawn(async move { router_server(router).await });
    let dealer_task = rt::spawn(async move { dealer_client(&endpoint).await });

    // Wait for both to complete concurrently
    let (router_result, dealer_result) =
        futures::join!(rt::join(router_task), rt::join(dealer_task));

    router_result?;
    dealer_result?;

    info!("\nExample completed successfully!");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn router_server(mut socket: RouterSocket) -> Result<()> {
    // Receive request
    info!("[ROUTER] Waiting for request...");
    let request = socket.recv_owned().await?;

    info!("[ROUTER] Received {} frames:", request.len());
    for (i, frame) in request.frames().enumerate() {
        info!(
            "[ROUTER]   Frame {}: {:?}",
            i,
            String::from_utf8_lossy(frame)
        );
    }

    // ROUTER messages from DEALER: [identity, ...payload]
    if let (Some(identity), Some(payload)) = (request.frame_bytes(0), request.get(1)) {
        info!(
            "[ROUTER] Identity: {:?}",
            String::from_utf8_lossy(&identity)
        );
        info!("[ROUTER] Message: {:?}", String::from_utf8_lossy(payload));

        // Send reply back to same identity
        let reply = Msg::from([identity, Bytes::from("Hello from ROUTER!")]);

        info!("[ROUTER] Sending reply with {} frames...", reply.len());
        socket.send(reply).await?;
        info!("[ROUTER] Reply sent");
    }

    socket.close().await?;
    info!("[ROUTER] Complete");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn dealer_client(endpoint: &str) -> Result<()> {
    info!("[DEALER] Connecting to {endpoint}");
    let mut socket = DealerSocket::new();
    socket.connect(endpoint).await?;

    // Send request. It waits for the connection to be up.
    info!("[DEALER] Sending request...");
    socket.send("Hello from DEALER!").await?;
    info!("[DEALER] Request sent");

    // Receive reply
    info!("[DEALER] Waiting for reply...");
    let reply = socket.recv().await?;

    info!("[DEALER] Received {} frames:", reply.len());
    for (i, frame) in reply.frames().enumerate() {
        info!(
            "[DEALER]   Frame {}: {:?}",
            i,
            String::from_utf8_lossy(frame)
        );
    }

    Ok(())
}
