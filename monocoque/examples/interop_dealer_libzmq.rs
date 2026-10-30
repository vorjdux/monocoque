//! Interoperability test: Monocoque DEALER <-> libzmq ROUTER
//!
//! This example demonstrates that Monocoque can communicate with libzmq.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_dealer_libzmq --features zmq
//! ```

use monocoque::rt::LocalRuntime;
use monocoque::zmq::DealerSocket;
use std::thread;
use std::time::Duration;
use tracing::info;

fn main() {
    info!("=== Monocoque <-> libzmq Interop Test ===\n");

    // Spawn libzmq ROUTER server in background thread
    let server_handle = thread::spawn(|| {
        let ctx = zmq::Context::new();
        let router = ctx.socket(zmq::ROUTER).unwrap();
        router.bind("tcp://127.0.0.1:5560").unwrap();
        info!("[libzmq ROUTER] Listening on tcp://127.0.0.1:5560");

        // Receive request with identity envelope
        let identity = router.recv_bytes(0).unwrap();
        let body = router.recv_bytes(0).unwrap();

        info!("[libzmq ROUTER] Received from client");
        info!("  Identity: {} bytes", identity.len());
        info!("  Body: {:?}", String::from_utf8_lossy(&body));

        // Send reply back to client
        router.send(&identity, zmq::SNDMORE).unwrap();
        router.send("Pong from libzmq", 0).unwrap();
        info!("[libzmq ROUTER] Sent reply\n");

        // Keep server alive for client to receive
        thread::sleep(Duration::from_millis(100));
    });

    // Run Monocoque DEALER client. Connecting does not wait for the server:
    // the socket dials in the background and the send waits for the peer.
    LocalRuntime::new().unwrap().block_on(async {
        info!("[Monocoque DEALER] Connecting to tcp://127.0.0.1:5560");

        let mut dealer = DealerSocket::new();
        dealer
            .connect("tcp://127.0.0.1:5560")
            .await
            .expect("Failed to connect");

        // Send request (the ROUTER adds the identity frame on its side)
        dealer
            .send("Ping from Monocoque")
            .await
            .expect("Failed to send");
        info!("[Monocoque DEALER] Sent request\n");

        // Receive reply
        let response = dealer.recv().await.expect("Failed to receive");

        info!("[Monocoque DEALER] Received response:");
        for (i, frame) in response.frames().enumerate() {
            info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
        }
    });

    server_handle.join().unwrap();

    info!("\nInterop test completed successfully!");
}
