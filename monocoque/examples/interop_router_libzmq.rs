//! Interoperability test: Monocoque ROUTER <-> libzmq DEALER
//!
//! This example demonstrates bidirectional compatibility with libzmq.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_router_libzmq --features zmq
//! ```

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, RouterSocket};
use std::sync::mpsc;
use std::thread;
use tracing::info;

fn main() {
    info!("=== Monocoque ROUTER <-> libzmq DEALER Test ===\n");

    // Spawn Monocoque ROUTER server in background thread
    let (ready_tx, ready_rx) = mpsc::channel();
    let server_handle = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async {
            let mut router = RouterSocket::new();
            router
                .bind("tcp://127.0.0.1:5561")
                .await
                .expect("Failed to bind");
            info!("[Monocoque ROUTER] Listening on tcp://127.0.0.1:5561");
            ready_tx.send(()).unwrap();

            // Receive request with identity envelope
            let msg = router.recv_owned().await.expect("Failed to receive");
            info!("[Monocoque ROUTER] Received message:");
            info!(
                "  Identity (frame 0): {:?}",
                msg.first().unwrap_or_default()
            );
            for (i, frame) in msg.frames().enumerate().skip(1) {
                info!("  Frame {i}: {:?}", String::from_utf8_lossy(frame));
            }

            // Reply to the client: the first frame is the identity to route
            // to, followed by an empty delimiter and the body.
            let identity = msg.frame_bytes(0).expect("missing identity");
            let reply = Msg::from([
                identity,
                Bytes::new(),
                Bytes::from_static(b"Reply from Monocoque ROUTER"),
            ]);
            router.send(reply).await.expect("Failed to send");
            info!("[Monocoque ROUTER] Sent reply\n");

            // Wait for the reply to be written before the runtime exits
            router.close().await.expect("Failed to close");
        });
    });

    // Wait until the server is bound
    ready_rx.recv().unwrap();

    // Run libzmq DEALER client
    info!("[libzmq DEALER] Connecting to tcp://127.0.0.1:5561");
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"CLIENT_123").unwrap();
    dealer.connect("tcp://127.0.0.1:5561").unwrap();
    info!("[libzmq DEALER] Connected with identity 'CLIENT_123'");

    // Send request
    dealer.send("Request from libzmq", 0).unwrap();
    info!("[libzmq DEALER] Sent request\n");

    // Receive reply: the empty delimiter, then the body
    let delimiter = dealer.recv_bytes(0).unwrap();
    assert!(delimiter.is_empty());
    let reply = dealer.recv_string(0).unwrap().unwrap();
    info!("[libzmq DEALER] Received reply: {:?}", reply);

    server_handle.join().unwrap();

    info!("\nInterop test completed successfully!");
}
