//! Interoperability test: Monocoque ROUTER ↔ libzmq DEALER
//!
//! This example demonstrates bidirectional compatibility with libzmq.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_router_libzmq --features zmq
//! ```

use bytes::Bytes;
use monocoque::zmq::RouterSocket;
use std::thread;
use std::time::Duration;

fn main() {
    println!("=== Monocoque ROUTER ↔ libzmq DEALER Test ===\n");

    // Spawn Monocoque ROUTER server in background thread
    let server_handle = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:5561")
                .await
                .expect("Failed to bind");
            println!("[Monocoque ROUTER] Listening on tcp://127.0.0.1:5561");

            let (stream, _) = listener.accept().await.expect("Failed to accept");
            println!("[Monocoque ROUTER] Client connected");

            let mut router = RouterSocket::from_stream(stream).await;

            // Receive request with identity envelope
            let msg = router.recv().await.expect("Failed to receive");
            println!("[Monocoque ROUTER] Received message:");
            println!("  Identity (frame 0): {} bytes", msg[0].len());
            if msg.len() > 1 {
                println!("  Delimiter (frame 1): {} bytes", msg[1].len());
            }
            if msg.len() > 2 {
                println!("  Body (frame 2): {:?}", String::from_utf8_lossy(&msg[2]));
            }

            // Send reply to client (identity + empty delimiter + body)
            // ROUTER must preserve the full envelope structure
            router
                .send(vec![
                    msg[0].clone(), // Echo back identity
                    Bytes::new(),   // Empty delimiter
                    Bytes::from_static(b"Reply from Monocoque ROUTER"),
                ])
                .await
                .expect("Failed to send");
            println!("[Monocoque ROUTER] Sent reply\n");

            // Give time for the message to be picked up by the task and written to network
            compio::time::sleep(Duration::from_millis(200)).await;

            drop(router);
        });
    });

    // Give server time to start
    thread::sleep(Duration::from_millis(50));

    // Run libzmq DEALER client
    println!("[libzmq DEALER] Connecting to tcp://127.0.0.1:5561");
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"CLIENT_123").unwrap();
    dealer.connect("tcp://127.0.0.1:5561").unwrap();
    println!("[libzmq DEALER] Connected with identity 'CLIENT_123'");

    // Send request
    dealer.send("Request from libzmq", 0).unwrap();
    println!("[libzmq DEALER] Sent request\n");

    // Receive reply
    let reply = dealer.recv_string(0).unwrap().unwrap();
    println!("[libzmq DEALER] Received reply: {:?}", reply);

    server_handle.join().unwrap();

    println!("\n✅ Interop test completed successfully!");
}
