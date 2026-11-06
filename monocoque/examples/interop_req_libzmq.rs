//! Interoperability test: Monocoque REQ <-> libzmq REP
//!
//! This example demonstrates that Monocoque REQ can communicate with libzmq REP.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_req_libzmq --features zmq
//! ```

use monocoque::rt::LocalRuntime;
use monocoque::zmq::ReqSocket;
use std::thread;
use std::time::Duration;
use tracing::info;

fn main() {
    info!("=== Monocoque REQ <-> libzmq REP Interop Test ===\n");

    // Spawn libzmq REP server in background thread
    let server_handle = thread::spawn(|| {
        let ctx = zmq::Context::new();
        let rep = ctx.socket(zmq::REP).unwrap();
        rep.bind("tcp://127.0.0.1:5561").unwrap();
        info!("[libzmq REP] Listening on tcp://127.0.0.1:5561");

        // Receive request
        let request = rep.recv_bytes(0).unwrap();
        info!(
            "[libzmq REP] Received request: {:?}",
            String::from_utf8_lossy(&request)
        );

        // Send reply
        rep.send("Reply from libzmq REP", 0).unwrap();
        info!("[libzmq REP] Sent reply\n");

        // Second round
        let request = rep.recv_bytes(0).unwrap();
        info!(
            "[libzmq REP] Received second request: {:?}",
            String::from_utf8_lossy(&request)
        );

        rep.send("Second reply from libzmq", 0).unwrap();
        info!("[libzmq REP] Sent second reply\n");

        // Keep server alive
        thread::sleep(Duration::from_millis(100));
    });

    // Run Monocoque REQ client. It reconnects in the background until the
    // server is up, so there is no need to wait for the bind.
    LocalRuntime::new().unwrap().block_on(async {
        info!("[Monocoque REQ] Connecting to tcp://127.0.0.1:5561");

        let mut socket = ReqSocket::new();
        socket
            .connect("tcp://127.0.0.1:5561")
            .await
            .expect("Failed to connect");

        // First request-reply cycle
        info!("[Monocoque REQ] Sending first request");
        socket
            .send("Request from Monocoque REQ")
            .await
            .expect("Failed to send");

        {
            let msg = socket.recv().await.expect("Failed to receive");
            info!("[Monocoque REQ] Received response:");
            for (i, frame) in msg.frames().enumerate() {
                info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
            }
        }

        // Second request-reply cycle
        info!("\n[Monocoque REQ] Sending second request");
        socket
            .send("Second request")
            .await
            .expect("Failed to send second request");

        let msg = socket.recv().await.expect("Failed to receive second reply");
        info!("[Monocoque REQ] Received second response:");
        for (i, frame) in msg.frames().enumerate() {
            info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
        }
    });

    server_handle.join().unwrap();

    info!("\nREQ interop test completed successfully!");
}
