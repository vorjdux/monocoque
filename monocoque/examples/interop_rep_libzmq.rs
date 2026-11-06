//! Interoperability test: Monocoque REP <-> libzmq REQ
//!
//! This example demonstrates that Monocoque REP can communicate with libzmq REQ.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_rep_libzmq --features zmq
//! ```

use monocoque::rt::LocalRuntime;
use monocoque::zmq::RepSocket;
use std::sync::mpsc;
use std::thread;
use tracing::info;

fn main() {
    info!("=== Monocoque REP <-> libzmq REQ Interop Test ===\n");

    // Spawn Monocoque REP server in background thread
    let (ready_tx, ready_rx) = mpsc::channel();
    let server_handle = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async {
            let mut socket = RepSocket::new();
            socket
                .bind("tcp://127.0.0.1:5562")
                .await
                .expect("Failed to bind");
            info!("[Monocoque REP] Listening on tcp://127.0.0.1:5562");
            ready_tx.send(()).unwrap();

            // First request-reply cycle
            {
                let msg = socket.recv().await.expect("Failed to receive");
                info!("[Monocoque REP] Received request:");
                for (i, frame) in msg.frames().enumerate() {
                    info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
                }
            }
            info!("[Monocoque REP] Sending reply");
            socket
                .send("Reply from Monocoque REP")
                .await
                .expect("Failed to send");

            // Second request-reply cycle
            {
                let msg = socket
                    .recv()
                    .await
                    .expect("Failed to receive second request");
                info!("\n[Monocoque REP] Received second request:");
                for (i, frame) in msg.frames().enumerate() {
                    info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
                }
            }
            info!("[Monocoque REP] Sending second reply");
            socket
                .send("Second reply from Monocoque")
                .await
                .expect("Failed to send second reply");

            // Wait for the reply to be written before the runtime exits
            socket.close().await.expect("Failed to close");
        });
    });

    // Wait until the server is bound
    ready_rx.recv().unwrap();

    // Run libzmq REQ client in main thread
    info!("[libzmq REQ] Connecting to tcp://127.0.0.1:5562");
    let ctx = zmq::Context::new();
    let req = ctx.socket(zmq::REQ).unwrap();
    req.connect("tcp://127.0.0.1:5562").unwrap();
    info!("[libzmq REQ] Connected\n");

    // First request-reply cycle
    info!("[libzmq REQ] Sending first request");
    req.send("Request from libzmq REQ", 0).unwrap();

    let reply = req.recv_bytes(0).unwrap();
    info!(
        "[libzmq REQ] Received reply: {:?}\n",
        String::from_utf8_lossy(&reply)
    );

    // Second request-reply cycle
    info!("[libzmq REQ] Sending second request");
    req.send("Second request from libzmq", 0).unwrap();

    let reply = req.recv_bytes(0).unwrap();
    info!(
        "[libzmq REQ] Received second reply: {:?}\n",
        String::from_utf8_lossy(&reply)
    );

    drop(req);

    server_handle.join().unwrap();

    info!("REP interop test completed successfully!");
}
