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
use monocoque_zmtp::{DealerSocket, RouterSocket};
use std::time::Duration;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Basic ROUTER-DEALER Example ===\n");

    // Use random port to avoid conflicts
    let port = 15570
        + (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            % 10000) as u16;
    let addr = format!("127.0.0.1:{}", port);
    println!("[INFO] Using address: {}\n", addr);

    let addr_clone = addr.clone();

    // Start ROUTER (server)
    let router_task = compio::runtime::spawn(async move { router_server(&addr).await });

    // Give server time to bind
    compio::time::sleep(Duration::from_millis(100)).await;

    // Start DEALER (client)
    let dealer_task = compio::runtime::spawn(async move { dealer_client(&addr_clone).await });

    // Wait for both to complete
    let router_result = router_task.await;
    let dealer_result = dealer_task.await;

    router_result?;
    dealer_result?;

    println!("\n✅ Example completed successfully!");
    Ok(())
}

async fn router_server(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("[ROUTER] Binding to {}", addr);
    let listener = TcpListener::bind(addr).await?;

    println!("[ROUTER] Waiting for connection...");
    let (stream, addr) = listener.accept().await?;
    println!("[ROUTER] Client connected from {}", addr);

    let socket = RouterSocket::new(stream).await;

    // Wait for handshake to complete
    compio::time::sleep(Duration::from_millis(500)).await;

    // Receive request
    println!("[ROUTER] Waiting for request...");
    let request = socket.recv().await?;

    println!("[ROUTER] Received {} frames:", request.len());
    for (i, frame) in request.iter().enumerate() {
        println!(
            "[ROUTER]   Frame {}: {:?}",
            i,
            String::from_utf8_lossy(frame)
        );
    }

    // ROUTER messages have envelope: [identity, empty_frame, ...payload]
    if request.len() >= 3 {
        let identity = &request[0];
        let payload = &request[2..];

        println!("[ROUTER] Identity: {:?}", String::from_utf8_lossy(identity));
        println!(
            "[ROUTER] Message: {:?}",
            String::from_utf8_lossy(&payload[0])
        );

        // Send reply back to same identity
        let reply = vec![
            identity.clone(),
            Bytes::new(), // Empty delimiter frame
            Bytes::from("Hello from ROUTER!"),
        ];

        println!("[ROUTER] Sending reply with {} frames...", reply.len());
        socket.send(reply).await?;
        println!("[ROUTER] Reply sent");
    }

    // Wait longer for DEALER to receive and process reply
    println!("[ROUTER] Waiting for DEALER to process reply...");
    compio::time::sleep(Duration::from_secs(3)).await;
    Ok(())
}

async fn dealer_client(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("[DEALER] Connecting to {}", addr);
    let stream = TcpStream::connect(addr).await?;
    println!("[DEALER] Connected!");

    let socket = DealerSocket::new(stream).await;

    // Wait for handshake to complete
    compio::time::sleep(Duration::from_millis(500)).await;

    // Send request
    let request = vec![Bytes::from("Hello from DEALER!")];
    println!("[DEALER] Sending request with {} frames...", request.len());
    socket.send(request).await?;
    println!("[DEALER] Request sent");

    // Receive reply
    println!("[DEALER] Waiting for reply...");
    let reply = socket.recv().await?;

    println!("[DEALER] Received {} frames:", reply.len());
    for (i, frame) in reply.iter().enumerate() {
        println!(
            "[DEALER]   Frame {}: {:?}",
            i,
            String::from_utf8_lossy(frame)
        );
    }

    // Keep connection alive to allow ROUTER to send reply
    compio::time::sleep(Duration::from_millis(1000)).await;
    Ok(())
}
