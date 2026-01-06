//! Interoperability test: Monocoque DEALER ↔ libzmq ROUTER
//!
//! This example demonstrates that Monocoque can communicate with libzmq.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_dealer_libzmq --features zmq
//! ```

use bytes::Bytes;
use monocoque::zmq::DealerSocket;
use std::thread;
use std::time::Duration;

fn main() {
    println!("=== Monocoque ↔ libzmq Interop Test ===\n");

    // Spawn libzmq ROUTER server in background thread
    let server_handle = thread::spawn(|| {
        let ctx = zmq::Context::new();
        let router = ctx.socket(zmq::ROUTER).unwrap();
        router.bind("tcp://127.0.0.1:5560").unwrap();
        println!("[libzmq ROUTER] Listening on tcp://127.0.0.1:5560");

        // Receive request with identity envelope
        let identity = router.recv_bytes(0).unwrap();
        let body = router.recv_bytes(0).unwrap();

        println!("[libzmq ROUTER] Received from client");
        println!("  Identity: {} bytes", identity.len());
        println!("  Body: {:?}", String::from_utf8_lossy(&body));

        // Send reply back to client
        router.send(&identity, zmq::SNDMORE).unwrap();
        router.send("Pong from libzmq", 0).unwrap();
        println!("[libzmq ROUTER] Sent reply\n");

        // Keep server alive for client to receive
        thread::sleep(Duration::from_millis(100));
    });

    // Give server time to bind
    thread::sleep(Duration::from_millis(50));

    // Run Monocoque DEALER client
    compio::runtime::Runtime::new().unwrap().block_on(async {
        println!("[Monocoque DEALER] Connecting to tcp://127.0.0.1:5560");

        let stream = compio::net::TcpStream::connect("127.0.0.1:5560")
            .await
            .expect("Failed to connect");

        let mut dealer = DealerSocket::from_stream(stream).await;
        println!("[Monocoque DEALER] Connected");

        // Send request (DEALER adds identity automatically)
        dealer
            .send(vec![Bytes::from_static(b"Ping from Monocoque")])
            .await
            .expect("Failed to send");
        println!("[Monocoque DEALER] Sent request\n");

        // Receive reply
        let response = dealer.recv().await.expect("Failed to receive");

        println!("[Monocoque DEALER] Received response:");
        for (i, frame) in response.iter().enumerate() {
            println!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
        }

        drop(dealer);
    });

    server_handle.join().unwrap();

    println!("\n✅ Interop test completed successfully!");
}
