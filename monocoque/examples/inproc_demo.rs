//! Inproc transport demonstration
//!
//! This example shows how to use the inproc transport for zero-copy
//! in-process messaging between sockets.
//!
//! # Features Demonstrated
//!
//! - Binding to inproc endpoints
//! - Connecting multiple clients to the same endpoint
//! - Zero-copy message passing (frames are shared `Bytes`, never copied)
//! - Cleanup and unbinding
//!
//! # Run
//!
//! ```sh
//! cargo run --example inproc_demo
//! ```

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{Msg, PullSocket, PushSocket};

const ENDPOINT: &str = "inproc://demo-endpoint";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Inproc Transport Demo ===\n");

    // 1. Bind to an inproc endpoint
    println!("1. Binding to '{ENDPOINT}'...");
    let mut server = PullSocket::new();
    let bound = server.bind(ENDPOINT).await?;
    println!("   Bound successfully to {bound}\n");

    // 2. Spawn a server task to receive messages
    println!("2. Starting server task...");
    let server_task = rt::spawn(async move {
        println!("   [Server] Listening for messages...");
        for count in 1..=4 {
            let Ok(msg) = server.recv().await else {
                break;
            };
            let frames: Vec<String> = msg
                .frames()
                .map(|f| {
                    if f.len() > 64 {
                        format!("<{} bytes>", f.len())
                    } else {
                        String::from_utf8_lossy(f).into_owned()
                    }
                })
                .collect();
            println!("   [Server] Received message #{count}: {frames:?}");
        }
        server.unbind(ENDPOINT).ok();
        println!("   [Server] Unbound endpoint, shutting down");
    });

    // 3. Connect multiple clients
    println!("\n3. Connecting clients...");
    let mut client1 = PushSocket::new();
    let mut client2 = PushSocket::new();
    let mut client3 = PushSocket::new();
    for client in [&mut client1, &mut client2, &mut client3] {
        client.connect(ENDPOINT).await?;
    }
    println!("   3 clients connected\n");

    // 4. Send messages from clients
    println!("4. Sending messages from clients...");
    client1.send("Hello from client 1").await?;
    client2
        .send(Msg::from([
            Bytes::from_static(b"Multi-part"),
            Bytes::from_static(b"message"),
            Bytes::from_static(b"from client 2"),
        ]))
        .await?;
    client3.send("Zero-copy message from client 3").await?;

    // 5. Send one more message to show zero-copy efficiency
    println!("\n5. Demonstrating zero-copy messaging...");
    let large_data = Bytes::from(vec![b'X'; 1024 * 100]); // 100 KB
    client1
        .send(Msg::from([Bytes::from_static(b"Large"), large_data]))
        .await?;
    println!("   Sent 100 KB message (the frame is shared, not copied)");

    // 6. Wait for the server to receive everything and unbind
    rt::join(server_task).await;

    println!("\n=== Demo Complete ===");
    Ok(())
}
