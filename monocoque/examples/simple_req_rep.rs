//! Simple REQ/REP test between Monocoque sockets
//!
//! Run this example:
//! ```bash
//! cargo run --example simple_req_rep --features zmq
//! ```

use bytes::Bytes;
use monocoque::zmq::{RepSocket, ReqSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use tracing::info;

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Monocoque REQ ↔ REP Simple Test ===\n");

    // Shared state
    let addr = Arc::new(std::sync::Mutex::new(String::new()));
    let addr_clone = addr.clone();
    let server_ready = Arc::new(AtomicBool::new(false));
    let server_ready_clone = server_ready.clone();

    // Spawn REP server in background thread
    let server_handle = thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("Failed to bind");
            let local_addr = listener.local_addr().expect("Failed to get local addr");
            info!("[REP] Listening on tcp://{}", local_addr);

            // Share the address with the client thread
            *addr_clone.lock().unwrap() = local_addr.to_string();
            server_ready_clone.store(true, Ordering::Release);

            let (stream, _) = listener.accept().await.expect("Failed to accept");
            let mut socket = RepSocket::from_tcp(stream).await.unwrap();
            info!("[REP] Client connected");

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

            info!("[REP] Request-reply cycle complete");
        });
    });

    // Wait for server to be ready
    while !server_ready.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(50)); // Extra settling time

    let server_addr = addr.lock().unwrap().clone();

    // Run REQ client in main thread
    compio::runtime::Runtime::new().unwrap().block_on(async {
        info!("[REQ] Connecting to tcp://{}", server_addr);

        let mut socket = ReqSocket::connect(&server_addr)
            .await
            .expect("Failed to connect");
        info!("[REQ] Connected (handshake complete)");

        // Send request
        info!("[REQ] Sending request");
        socket
            .send(vec![Bytes::from_static(b"Request from REQ")])
            .await
            .expect("Failed to send");

        // Receive reply
        info!("[REQ] Waiting for reply");
        let response = socket.recv().await;
        if let Some(msg) = response {
            info!("[REQ] Received response:");
            for (i, frame) in msg.iter().enumerate() {
                info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
            }
        } else {
            info!("[REQ] No response received");
        }

        info!("[REQ] Request-reply cycle complete");
    });

    // Wait for server thread to complete
    info!("Waiting for server thread to finish...");
    server_handle.join().expect("Server thread panicked");

    info!("\n✅ Simple test completed successfully!");
}
