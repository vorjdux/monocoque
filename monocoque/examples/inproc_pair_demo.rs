//! Demonstration of inproc PAIR socket pattern.
//!
//! Shows zero-copy bidirectional communication between two PAIR sockets
//! in the same process using the inproc transport.

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::PairSocket;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Inproc PAIR Pattern Demo ===\n");

    // Server binds, client connects. A PAIR link carries messages both ways.
    let mut server = PairSocket::new();
    server.bind("inproc://pair-endpoint").await?;
    println!("1. Server bound to 'inproc://pair-endpoint'");

    let mut client = PairSocket::new();
    client.connect("inproc://pair-endpoint").await?;
    println!("2. Client connected\n");

    // Server task
    let server_task = rt::spawn(async move {
        println!("[Server] Waiting for messages...");

        for i in 1..=3 {
            // Receive from client
            match server.recv().await {
                Ok(msg) => println!(
                    "[Server] Received from client: {:?}",
                    String::from_utf8_lossy(msg.first().unwrap_or_default())
                ),
                Err(e) => {
                    println!("[Server] Receive failed: {e}");
                    return;
                }
            }

            // Send response to client
            server.send(format!("Server response #{i}")).await.ok();
        }

        println!("[Server] Done");
    });

    // Client sends messages
    for i in 1..=3 {
        let msg = format!("Client message #{i}");
        println!("[Client] Sending: {msg}");
        client.send(msg).await?;

        // Receive response
        let response = client.recv().await?;
        println!(
            "[Client] Got response: {:?}",
            String::from_utf8_lossy(response.first().unwrap_or_default())
        );
    }

    rt::join(server_task).await;

    println!("\n=== Demo Complete ===");

    Ok(())
}
