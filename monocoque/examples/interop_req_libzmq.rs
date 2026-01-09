//! Interoperability test: Monocoque REQ ↔ libzmq REP
//!
//! This example demonstrates that Monocoque REQ can communicate with libzmq REP.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_req_libzmq --features zmq
//! ```

use bytes::Bytes;
use monocoque_zmtp::req::ReqSocket;
use std::thread;
use std::time::Duration;
use tracing::info;

fn main() {
    info!("=== Monocoque REQ ↔ libzmq REP Interop Test ===\n");

    // Spawn libzmq REP server in background thread
    let server_handle = thread::spawn(|| {
        let ctx = zmq::Context::new();
        let rep = ctx.socket(zmq::REP).unwrap();
        rep.bind("tcp://127.0.0.1:5561").unwrap();
        info!("[libzmq REP] Listening on tcp://127.0.0.1:5561");

        // Receive request
        let request = rep.recv_bytes(0).unwrap();
        info!("[libzmq REP] Received request: {:?}", String::from_utf8_lossy(&request));

        // Send reply
        rep.send("Reply from libzmq REP", 0).unwrap();
        info!("[libzmq REP] Sent reply\n");

        // Second round
        let request = rep.recv_bytes(0).unwrap();
        info!("[libzmq REP] Received second request: {:?}", String::from_utf8_lossy(&request));

        rep.send("Second reply from libzmq", 0).unwrap();
        info!("[libzmq REP] Sent second reply\n");

        // Keep server alive
        thread::sleep(Duration::from_millis(100));
    });

    // Give server time to bind
    thread::sleep(Duration::from_millis(50));

    // Run Monocoque REQ client
    compio::runtime::Runtime::new().unwrap().block_on(async {
        info!("[Monocoque REQ] Connecting to tcp://127.0.0.1:5561");

        let stream = compio::net::TcpStream::connect("127.0.0.1:5561")
            .await
            .expect("Failed to connect");

        let mut socket = ReqSocket::new(stream).await.unwrap();
        info!("[Monocoque REQ] Connected\n");

        // First request-reply cycle
        info!("[Monocoque REQ] Sending first request");
        socket
            .send(vec![Bytes::from_static(b"Request from Monocoque REQ")])
            .await
            .expect("Failed to send");

        let response = socket.recv().await.expect("Failed to receive");
        info!("[Monocoque REQ] Received response:");
        if let Some(msg) = response {
            for (i, frame) in msg.iter().enumerate() {
                info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
            }
        }

        // Second request-reply cycle
        info!("\n[Monocoque REQ] Sending second request");
        socket
            .send(vec![Bytes::from_static(b"Second request")])
            .await
            .expect("Failed to send second request");

        let response = socket.recv().await.expect("Failed to receive second reply");
        info!("[Monocoque REQ] Received second response:");
        if let Some(msg) = response {
            for (i, frame) in msg.iter().enumerate() {
                info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
            }
        }

        drop(socket);
    });

    server_handle.join().unwrap();

    info!("\n✅ REQ interop test completed successfully!");
}
