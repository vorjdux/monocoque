//! Simple REQ/REP test between Monocoque sockets
//!
//! Run this example:
//! ```bash
//! cargo run --example simple_req_rep
//! ```

use bytes::Bytes;
use monocoque::zmq::{RepSocket, ReqSocket};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::info;

fn main() {
    info!("=== Monocoque REQ ↔ REP Simple Test ===\n");

    // Spawn REP server in background thread
    let addr = Arc::new(std::sync::Mutex::new(String::new()));
    let addr_clone = addr.clone();
    
    let server_handle = thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("Failed to bind");
            let local_addr = listener.local_addr().expect("Failed to get local addr");
            info!("[REP] Listening on tcp://{}", local_addr);
            
            // Share the address with the client thread
            *addr_clone.lock().unwrap() = local_addr.to_string();

            let (stream, _) = listener.accept().await.expect("Failed to accept");
            let mut socket = RepSocket::from_stream(stream).await.unwrap();
            info!("[REP] Client connected\n");

            // First request-reply cycle
            if let Some(request) = socket.recv().await {
                info!("[REP] Received request:");
                for (i, frame) in request.iter().enumerate() {
                    info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
                }

                info!("[REP] Sending reply");
                socket
                    .send(vec![Bytes::from_static(b"Reply from REP")])
                    .await
                    .expect("Failed to send");
            }

            info!("[REP] Done");
            
            drop(socket);
            
            // Keep connection alive briefly
            compio::time::sleep(Duration::from_millis(50)).await;
        });
    });

    // Give server time to start and get address
    // Note: This settle time is standard practice in ZeroMQ implementations.
    // libzmq uses SETTLE_TIME=300ms, zmq.rs uses 100ms sleeps.
    // This is only needed for localhost TCP tests; real network latency
    // naturally provides this settling period.
    thread::sleep(Duration::from_millis(100));
    
    let server_addr = addr.lock().unwrap().clone();

    // Run REQ client in main thread
    compio::runtime::Runtime::new().unwrap().block_on(async {
        info!("[REQ] Connecting to tcp://{}", server_addr);

        let mut socket = ReqSocket::connect(&server_addr)
            .await
            .expect("Failed to connect");
        info!("[REQ] Connected (handshake complete)\n");

        // Send request
        info!("[REQ] Sending request");
        socket
            .send(vec![Bytes::from_static(b"Request from REQ")])
            .await
            .expect("Failed to send");

        // Receive reply
        let response = socket.recv().await;
        if let Some(msg) = response {
            info!("[REQ] Received response:");
            for (i, frame) in msg.iter().enumerate() {
                info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
            }
        }

        info!("[REQ] Done");
        
        drop(socket);
        
        // Small delay to let messages flush
        compio::time::sleep(Duration::from_millis(50)).await;
    });

    server_handle.join().unwrap();

    info!("\n✅ Simple test completed successfully!");
}
