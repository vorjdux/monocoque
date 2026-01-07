/// Complete Request-Reply Pattern Example
/// 
/// Demonstrates ROUTER (server) and DEALER (client) pattern
/// This is a common pattern for RPC-style services

fn main() {
    use monocoque_zmtp::router::RouterSocket;
    use monocoque_zmtp::dealer::DealerSocket;
    use bytes::Bytes;
    use std::thread;
    use std::sync::mpsc;

    println!("=== Request-Reply Pattern ===\n");
    println!("Server: ROUTER socket (identity-based routing)");
    println!("Client: DEALER socket (anonymous identity)\n");

    // Channel to communicate server address
    let (addr_tx, addr_rx) = mpsc::channel();

    // Server thread
    let server = thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            // Bind to port 0 to get a random available port
            let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            println!("[Server] Starting on {}...", addr);
            
            // Send address to client thread
            addr_tx.send(addr).unwrap();
            
            println!("[Server] Waiting for clients...");
            let (stream, client_addr) = listener.accept().await.unwrap();
            println!("[Server] Client connected from {}", client_addr);
            
            let router = RouterSocket::new(stream).await;
            
            // Process 3 requests
            for i in 1..=3 {
                let request = router.recv().await.unwrap();
                // ROUTER message format: [routing_id, empty_delimiter, ...message_frames...]
                let client_id = &request[0];
                let message = &request[2]; // Skip empty delimiter at index 1
                
                println!("[Server] Request {}: {:?} from client {:?}", 
                         i,
                         String::from_utf8_lossy(message),
                         String::from_utf8_lossy(client_id));
                
                // Send reply back to specific client
                let reply = format!("Response #{i}");
                router.send(vec![
                    client_id.clone(),
                    Bytes::from(reply),
                ]).await.unwrap();
                
                println!("[Server] Sent response {i}");
            }
            
            println!("[Server] Done processing requests");
        });
    });

    // Wait for server to start and get its address
    let server_addr = addr_rx.recv().unwrap();

    // Client thread
    let client = thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            println!("[Client] Connecting to server...");
            let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
            println!("[Client] Connected!");
            
            let dealer = DealerSocket::new(stream).await;
            
            // Send 3 requests
            for i in 1..=3 {
                let request = format!("Request #{i}");
                println!("[Client] Sending: {request}");
                
                dealer.send(vec![Bytes::from(request)]).await.unwrap();
                
                let response = dealer.recv().await.unwrap();
                println!("[Client] Received: {:?}", 
                         String::from_utf8_lossy(&response[0]));
                
                // Small delay between requests
                compio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            
            println!("[Client] Done!");
        });
    });

    server.join().unwrap();
    client.join().unwrap();
    
    println!("\n✅ Request-Reply pattern complete!");
    println!("\nThis demonstrates:");
    println!("- ROUTER socket receiving messages with client identity");
    println!("- ROUTER routing replies back to specific clients");
    println!("- DEALER socket for client-side requests");
    println!("- Full duplex communication over single connection");
}
