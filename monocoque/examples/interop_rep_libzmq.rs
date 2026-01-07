//! Interoperability test: Monocoque REP ↔ libzmq REQ
//!
//! This example demonstrates that Monocoque REP can communicate with libzmq REQ.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_rep_libzmq --features zmq
//! ```

use bytes::Bytes;
use monocoque_zmtp::rep::RepSocket;
use std::thread;
use std::time::Duration;

fn main() {
    println!("=== Monocoque REP ↔ libzmq REQ Interop Test ===\n");

    // Spawn Monocoque REP server in background thread
    let server_handle = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:5562")
                .await
                .expect("Failed to bind");
            println!("[Monocoque REP] Listening on tcp://127.0.0.1:5562");

            let (stream, _) = listener.accept().await.expect("Failed to accept");
            let socket = RepSocket::new(stream).await;
            println!("[Monocoque REP] Client connected\n");

            // First request-reply cycle
            let request = socket.recv().await.expect("Failed to receive");
            if let Some(msg) = request {
                println!("[Monocoque REP] Received request:");
                for (i, frame) in msg.iter().enumerate() {
                    println!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
                }

                println!("[Monocoque REP] Sending reply");
                socket
                    .send(vec![Bytes::from_static(b"Reply from Monocoque REP")])
                    .await
                    .expect("Failed to send");
            }

            // Second request-reply cycle
            let request = socket.recv().await.expect("Failed to receive second request");
            if let Some(msg) = request {
                println!("\n[Monocoque REP] Received second request:");
                for (i, frame) in msg.iter().enumerate() {
                    println!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
                }

                println!("[Monocoque REP] Sending second reply");
                socket
                    .send(vec![Bytes::from_static(b"Second reply from Monocoque")])
                    .await
                    .expect("Failed to send second reply");
            }

            // Keep connection alive
            compio::time::sleep(Duration::from_millis(100)).await;
        });
    });

    // Give server time to start
    thread::sleep(Duration::from_millis(50));

    // Run libzmq REQ client in main thread
    println!("[libzmq REQ] Connecting to tcp://127.0.0.1:5562");
    let ctx = zmq::Context::new();
    let req = ctx.socket(zmq::REQ).unwrap();
    req.connect("tcp://127.0.0.1:5562").unwrap();
    println!("[libzmq REQ] Connected\n");

    // First request-reply cycle
    println!("[libzmq REQ] Sending first request");
    req.send("Request from libzmq REQ", 0).unwrap();

    let reply = req.recv_bytes(0).unwrap();
    println!("[libzmq REQ] Received reply: {:?}\n", String::from_utf8_lossy(&reply));

    // Second request-reply cycle
    println!("[libzmq REQ] Sending second request");
    req.send("Second request from libzmq", 0).unwrap();

    let reply = req.recv_bytes(0).unwrap();
    println!("[libzmq REQ] Received second reply: {:?}\n", String::from_utf8_lossy(&reply));

    drop(req);

    server_handle.join().unwrap();

    println!("✅ REP interop test completed successfully!");
}
