//! Demonstration of inproc PAIR socket pattern.
//!
//! Shows zero-copy bidirectional communication between two PAIR sockets
//! in the same process using the inproc transport.

use bytes::Bytes;
use monocoque_core::inproc::{bind_inproc, connect_inproc};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Inproc PAIR Pattern Demo ===\n");

    // Create bidirectional channels for PAIR pattern
    // Server binds and gets (tx_server, rx_server)
    let (tx_server, rx_server) = bind_inproc("inproc://pair-endpoint")?;
    println!("1. Server bound to 'inproc://pair-endpoint'");

    // Client connects and creates its own bidirectional channel
    let tx_client_to_server = connect_inproc("inproc://pair-endpoint")?;
    let (tx_server_to_client, rx_client) = flume::unbounded();
    println!("2. Client connected\n");

    // For true bidirectional PAIR, we need to exchange the return channel
    // Send the client's receiver channel to the server
    tx_client_to_server.send(vec![Bytes::from("__PAIR_HANDSHAKE__")])?;
    
    // Server thread
    let server_thread = thread::spawn(move || {
        println!("[Server] Waiting for messages...");
        
        // Receive handshake (in real implementation, this would exchange channel info)
        if let Ok(msg) = rx_server.recv() {
            if msg.len() == 1 && msg[0] == "__PAIR_HANDSHAKE__" {
                println!("[Server] Pair established");
            }
        }

        // Simulate bidirectional communication
        for i in 1..=3 {
            // Receive from client
            if let Ok(msg) = rx_server.recv() {
                println!("[Server] Received from client: {:?}", 
                    String::from_utf8_lossy(&msg[0]));
            }
            
            // Send response to client
            let response = format!("Server response #{}", i);
            tx_server_to_client.send(vec![Bytes::from(response)]).ok();
        }
        
        println!("[Server] Done");
    });

    // Client sends messages
    thread::sleep(Duration::from_millis(100));
    
    for i in 1..=3 {
        let msg = format!("Client message #{}", i);
        println!("[Client] Sending: {}", msg);
        tx_client_to_server.send(vec![Bytes::from(msg)])?;
        
        // Receive response
        if let Ok(response) = rx_client.recv() {
            println!("[Client] Got response: {:?}", 
                String::from_utf8_lossy(&response[0]));
        }
        
        thread::sleep(Duration::from_millis(50));
    }

    server_thread.join().unwrap();

    println!("\n=== Demo Complete ===");
    println!("\nNote: This demo shows the challenge of implementing bidirectional");
    println!("inproc patterns. A full implementation would need:");
    println!("  - Channel exchange protocol");
    println!("  - Bidirectional InprocStream adapter");
    println!("  - Proper PAIR semantics");

    Ok(())
}
