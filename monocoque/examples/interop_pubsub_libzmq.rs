//! Interoperability test: Monocoque PUB ↔ libzmq SUB
//!
//! This example demonstrates pub/sub compatibility with libzmq.
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_pubsub_libzmq --features zmq
//! ```

use bytes::Bytes;
use monocoque::zmq::PubSocket;
use std::thread;
use std::time::Duration;

fn main() {
    println!("=== Monocoque PUB ↔ libzmq SUB Test ===\n");

    // Spawn libzmq SUB client in background thread
    let subscriber_handle = thread::spawn(|| {
        thread::sleep(Duration::from_millis(50)); // Let publisher start first

        let ctx = zmq::Context::new();
        let sub = ctx.socket(zmq::SUB).unwrap();
        sub.connect("tcp://127.0.0.1:5562").unwrap();
        sub.set_subscribe(b"topic").unwrap();
        println!("[libzmq SUB] Connected and subscribed to 'topic.*'");

        // Receive messages
        for i in 1..=3 {
            let msg = sub.recv_string(0).unwrap().unwrap();
            println!("[libzmq SUB] Received message {}: {:?}", i, msg);
        }
    });

    // Run Monocoque PUB server
    compio::runtime::Runtime::new().unwrap().block_on(async {
        let listener = compio::net::TcpListener::bind("127.0.0.1:5562")
            .await
            .expect("Failed to bind");
        println!("[Monocoque PUB] Listening on tcp://127.0.0.1:5562");

        let (stream, _) = listener.accept().await.expect("Failed to accept");
        println!("[Monocoque PUB] Subscriber connected\n");

        let mut pub_socket = PubSocket::from_stream(stream).await;

        // Give subscriber time to send subscription
        compio::time::sleep(Duration::from_millis(100)).await;

        // Publish messages
        for i in 1..=3 {
            let message = format!("topic.event.{}", i);
            pub_socket
                .send(vec![Bytes::from(message.clone())])
                .await
                .expect("Failed to publish");
            println!("[Monocoque PUB] Published: {:?}", message);
            compio::time::sleep(Duration::from_millis(10)).await;
        }

        // Give subscriber time to receive
        compio::time::sleep(Duration::from_millis(100)).await;

        drop(pub_socket);
    });

    subscriber_handle.join().unwrap();

    println!("\n✅ PUB/SUB interop test completed successfully!");
}
