/// Complete Request-Reply Pattern Example
/// 
/// Demonstrates ROUTER (server) and DEALER (client) pattern
/// This is a common pattern for RPC-style services

#[cfg(feature = "runtime")]
fn main() {
    use monocoque_zmtp::router::RouterSocket;
    use monocoque_zmtp::dealer::DealerSocket;
    use bytes::Bytes;
    use std::thread;

    println!("=== Request-Reply Pattern ===\n");
    println!("Server: ROUTER socket (identity-based routing)");
    println!("Client: DEALER socket (anonymous identity)\n");

    // Server thread
    let server = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            println!("[Server] Starting on 127.0.0.1:9000...");
            let listener = compio::net::TcpListener::bind("127.0.0.1:9000").await.unwrap();
            
            println!("[Server] Waiting for clients...");
            let (stream, addr) = listener.accept().await.unwrap();
            println!("[Server] Client connected from {}", addr);
            
            let router = RouterSocket::new(stream);
            
            // Process 3 requests
            for i in 1..=3 {
                let request = router.recv().await.unwrap();
                let client_id = &request[0];
                let message = &request[1];
                
                println!("[Server] Request {}: {:?} from client {:?}", 
                         i,
                         String::from_utf8_lossy(message),
                         String::from_utf8_lossy(client_id));
                
                // Send reply back to specific client
                let reply = format!("Response #{}", i);
                router.send(vec![
                    client_id.clone(),
                    Bytes::from(reply),
                ]).await.unwrap();
                
                println!("[Server] Sent response {}", i);
            }
            
            println!("[Server] Done processing requests");
        });
    });

    // Give server time to start
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Client thread
    let client = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            println!("[Client] Connecting to server...");
            let stream = compio::net::TcpStream::connect("127.0.0.1:9000").await.unwrap();
            println!("[Client] Connected!");
            
            let dealer = DealerSocket::new(stream);
            
            // Send 3 requests
            for i in 1..=3 {
                let request = format!("Request #{}", i);
                println!("[Client] Sending: {}", request);
                
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

#[cfg(not(feature = "runtime"))]
fn main() {
    println!("This example requires the 'runtime' feature.");
    println!("Run with: cargo run --example request_reply --features runtime");
}
