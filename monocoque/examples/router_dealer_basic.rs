use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
/// Basic ROUTER-DEALER Example
///
/// Demonstrates a working request-response pattern where:
/// - ROUTER socket binds and waits for requests
/// - DEALER socket connects and sends a request
/// - ROUTER receives, processes, and replies
/// - DEALER receives the response
///
/// This validates the complete ZMTP implementation including handshake and messaging.
use monocoque::zmq::{DealerSocket, RouterSocket};
use std::time::Duration;
use tracing::info;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Basic ROUTER-DEALER Example ===\n");

    // Use random port to avoid conflicts
    let port = 15570
        + (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            % 10000) as u16;
    let addr = format!("127.0.0.1:{port}");
    info!("[INFO] Using address: {addr}\n");

    let addr_clone = addr.clone();

    // Start ROUTER (server)
    let router_task = compio::runtime::spawn(async move { router_server(&addr).await });

    // Give server time to bind
    compio::time::sleep(Duration::from_millis(100)).await;

    // Start DEALER (client)
    let dealer_task = compio::runtime::spawn(async move { dealer_client(&addr_clone).await });

    // Wait for both to complete concurrently
    let (router_result, dealer_result) = futures::join!(router_task, dealer_task);

    router_result?;
    dealer_result?;

    info!("\n✅ Example completed successfully!");
    Ok(())
}

async fn router_server(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    info!("[ROUTER] Binding to {addr}");
    let listener = TcpListener::bind(addr).await?;

    info!("[ROUTER] Waiting for connection...");
    let (stream, addr) = listener.accept().await?;
    info!("[ROUTER] Client connected from {addr}");

    let mut socket = RouterSocket::from_tcp(stream).await?;

    // Receive request
    info!("[ROUTER] Waiting for request...");
    let request = socket.recv().await.ok_or("Connection closed")?;

    info!("[ROUTER] Received {} frames:", request.len());
    for (i, frame) in request.iter().enumerate() {
        info!(
            "[ROUTER]   Frame {}: {:?}",
            i,
            String::from_utf8_lossy(frame)
        );
    }

    // ROUTER messages from DEALER: [identity, ...payload]
    if request.len() >= 2 {
        let identity = &request[0];
        let payload = &request[1];

        info!("[ROUTER] Identity: {:?}", String::from_utf8_lossy(identity));
        info!(
            "[ROUTER] Message: {:?}",
            String::from_utf8_lossy(payload)
        );

        // Send reply back to same identity
        let reply = vec![
            identity.clone(),
            Bytes::from("Hello from ROUTER!"),
        ];

        info!("[ROUTER] Sending reply with {} frames...", reply.len());
        socket.send(reply).await?;
        info!("[ROUTER] Reply sent");
    }

    info!("[ROUTER] Complete");
    Ok(())
}

async fn dealer_client(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    info!("[DEALER] Connecting to {addr}");
    let stream = TcpStream::connect(addr).await?;
    info!("[DEALER] Connected!");

    let mut socket = DealerSocket::from_tcp(stream).await?;

    // Send request
    let request = vec![Bytes::from("Hello from DEALER!")];
    info!("[DEALER] Sending request with {} frames...", request.len());
    socket.send(request).await?;
    info!("[DEALER] Request sent");

    // Receive reply
    info!("[DEALER] Waiting for reply...");
    let reply = socket.recv().await.ok_or("Connection closed")?;

    info!("[DEALER] Received {} frames:", reply.len());
    for (i, frame) in reply.iter().enumerate() {
        info!(
            "[DEALER]   Frame {}: {:?}",
            i,
            String::from_utf8_lossy(frame)
        );
    }

    Ok(())
}
