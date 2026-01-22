//! Inproc transport demonstration
//!
//! This example shows how to use the inproc transport for zero-copy
//! in-process messaging between sockets.
//!
//! # Features Demonstrated
//!
//! - Binding to inproc endpoints
//! - Connecting multiple clients to the same endpoint
//! - Zero-copy message passing via channels
//! - Cleanup and unbinding
//!
//! # Run
//!
//! ```sh
//! cargo run --example inproc_demo
//! ```

use bytes::Bytes;
use monocoque_core::inproc::{bind_inproc, connect_inproc, list_inproc_endpoints, unbind_inproc};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Inproc Transport Demo ===\n");

    // 1. Bind to an inproc endpoint
    println!("1. Binding to 'inproc://demo-endpoint'...");
    let (server_tx, mut server_rx) = bind_inproc("inproc://demo-endpoint")?;
    println!("   ✓ Bound successfully\n");

    // 2. List endpoints
    println!("2. Current inproc endpoints:");
    for endpoint in list_inproc_endpoints() {
        println!("   - inproc://{}", endpoint);
    }
    println!();

    // 3. Spawn a server task to receive messages
    println!("3. Starting server thread...");
    let server_thread = thread::spawn(move || {
        println!("   [Server] Listening for messages...");
        let mut count = 0;
        while let Ok(msg) = server_rx.recv() {
            count += 1;
            println!(
                "   [Server] Received message #{}: {:?}",
                count,
                msg.iter()
                    .map(|b| String::from_utf8_lossy(b).to_string())
                    .collect::<Vec<_>>()
            );
        }
        println!("   [Server] Shutting down (received {} messages)", count);
    });

    // Small delay to ensure server is ready
    thread::sleep(Duration::from_millis(10));

    // 4. Connect multiple clients
    println!("\n4. Connecting clients...");
    let client1 = connect_inproc("inproc://demo-endpoint")?;
    let client2 = connect_inproc("inproc://demo-endpoint")?;
    let client3 = connect_inproc("inproc://demo-endpoint")?;
    println!("   ✓ 3 clients connected\n");

    // 5. Send messages from clients
    println!("5. Sending messages from clients...");

    client1
        .send(vec![Bytes::from("Hello from client 1")])
        .map_err(|_| "send failed")?;
    thread::sleep(Duration::from_millis(10));

    client2
        .send(vec![
            Bytes::from("Multi-part"),
            Bytes::from("message"),
            Bytes::from("from client 2"),
        ])
        .map_err(|_| "send failed")?;
    thread::sleep(Duration::from_millis(10));

    client3
        .send(vec![Bytes::from("🚀 Zero-copy message from client 3")])
        .map_err(|_| "send failed")?;
    thread::sleep(Duration::from_millis(10));

    // 6. Send one more message to show zero-copy efficiency
    println!("\n6. Demonstrating zero-copy messaging...");
    let large_data = Bytes::from(vec![b'X'; 1024 * 100]); // 100 KB
    client1
        .send(vec![Bytes::from("Large"), large_data.clone()])
        .map_err(|_| "send failed")?;
    println!("   ✓ Sent 100 KB message (zero-copy via Arc)");
    thread::sleep(Duration::from_millis(10));

    // 7. Cleanup
    println!("\n7. Cleaning up...");

    // Drop all senders to close the channel
    drop(server_tx);
    drop(client1);
    drop(client2);
    drop(client3);

    // Wait for server to finish
    server_thread.join().unwrap();

    // Unbind the endpoint
    unbind_inproc("inproc://demo-endpoint")?;
    println!("   ✓ Unbound endpoint");

    // Verify endpoint list is empty
    let endpoints = list_inproc_endpoints();
    println!("\n8. Final endpoint list: {:?}", endpoints);
    assert!(endpoints.is_empty(), "All endpoints should be unbound");

    println!("\n=== Demo Complete ===");
    Ok(())
}
