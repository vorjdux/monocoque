//! Demonstrates endpoint parsing for TCP and IPC transports.
//!
//! This example shows how to use the Endpoint abstraction to parse
//! and work with different transport types.

use monocoque::zmq::{Endpoint, EndpointError};

fn main() -> Result<(), EndpointError> {
    // Parse TCP IPv4 endpoint
    let tcp4 = Endpoint::parse("tcp://127.0.0.1:5555")?;
    println!("Parsed TCP IPv4: {}", tcp4);
    
    // Parse TCP IPv6 endpoint
    let tcp6 = Endpoint::parse("tcp://[::1]:6666")?;
    println!("Parsed TCP IPv6: {}", tcp6);
    
    // Parse IPC endpoint (Unix only)
    #[cfg(unix)]
    {
        let ipc = Endpoint::parse("ipc:///tmp/my_socket.sock")?;
        println!("Parsed IPC: {}", ipc);
    }
    
    // Demonstrate error handling
    match Endpoint::parse("http://invalid.example.com") {
        Ok(_) => println!("Unexpected success"),
        Err(e) => println!("Expected error: {}", e),
    }
    
    // Round-trip conversion (parse -> display -> parse)
    let original = "tcp://192.168.1.100:7777";
    let endpoint = Endpoint::parse(original)?;
    let serialized = endpoint.to_string();
    let reparsed = Endpoint::parse(&serialized)?;
    
    println!("\nRound-trip test:");
    println!("  Original:   {}", original);
    println!("  Serialized: {}", serialized);
    println!("  Reparsed:   {}", reparsed);
    assert_eq!(serialized, reparsed.to_string());
    
    println!("\n✅ All endpoint parsing examples completed successfully");
    Ok(())
}
