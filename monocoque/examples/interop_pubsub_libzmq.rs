//! Interoperability test: Monocoque PUB <-> libzmq SUB
//!
//! This example demonstrates pub/sub compatibility with libzmq.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_pubsub_libzmq --features zmq
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::PubSocket;
use std::thread;
use std::time::Duration;
use tracing::info;

fn main() {
    info!("=== Monocoque PUB <-> libzmq SUB Test ===\n");

    // Spawn libzmq SUB client in background thread
    let subscriber_handle = thread::spawn(|| {
        thread::sleep(Duration::from_millis(50)); // Let publisher start first

        let ctx = zmq::Context::new();
        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.connect("tcp://127.0.0.1:5562").unwrap();
        sub.set_subscribe(b"topic").unwrap();
        info!("[libzmq SUB] Connected and subscribed to 'topic.*'");

        // Receive messages
        for i in 1..=3 {
            let msg = sub.recv_string(0).unwrap().unwrap();
            info!("[libzmq SUB] Received message {}: {:?}", i, msg);
        }
    });

    // Run Monocoque PUB server
    LocalRuntime::new().unwrap().block_on(async {
        let mut pub_socket = PubSocket::new();
        pub_socket
            .bind("tcp://127.0.0.1:5562")
            .await
            .expect("Failed to bind");
        info!("[Monocoque PUB] Listening on tcp://127.0.0.1:5562");

        // A PUB drops messages nobody is subscribed to, so publish only once
        // the subscriber's subscription has arrived.
        loop {
            let reached = pub_socket
                .send("topic.event.0")
                .await
                .expect("Failed to publish");
            if reached > 0 {
                break;
            }
            rt::sleep(Duration::from_millis(10)).await;
        }
        info!("[Monocoque PUB] Subscriber connected\n");

        // Publish messages
        for i in 1..=3 {
            let message = format!("topic.event.{i}");
            pub_socket
                .send(message.clone())
                .await
                .expect("Failed to publish");
            info!("[Monocoque PUB] Published: {:?}", message);
        }

        // Give subscriber time to receive
        rt::sleep(Duration::from_millis(100)).await;
        pub_socket.close().await.expect("Failed to close");
    });

    subscriber_handle.join().unwrap();

    info!("\nPUB/SUB interop test completed successfully!");
}
