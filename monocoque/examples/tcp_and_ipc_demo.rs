//! Demonstrates both TCP and IPC transport support.
//!
//! This example shows how to use both TCP and Unix domain sockets
//! with the same socket types.
//!
//! Run with:
//! ```bash
//! cargo run --example tcp_and_ipc_demo --features zmq
//! ```

use monocoque::zmq::DealerSocket;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== TCP and IPC Transport Demo ===\n");

    // TCP example
    println!("1. TCP Transport:");
    println!("   Creating DEALER socket with TCP...");
    match DealerSocket::connect("tcp://127.0.0.1:5555").await {
        Ok(_socket) => {
            println!("   ✓ TCP socket created successfully");
            println!("   (Would connect if server was running)\n");
        }
        Err(e) => {
            println!("   ✗ Expected connection error (no server): {}\n", e);
        }
    }

    // IPC example (Unix only)
    #[cfg(unix)]
    {
        println!("2. IPC Transport (Unix domain sockets):");
        println!("   Creating DEALER socket with IPC...");
        
        // Clean up any existing socket file
        let socket_path = "/tmp/monocoque_test.sock";
        let _ = std::fs::remove_file(socket_path);
        
        match DealerSocket::connect_ipc(socket_path).await {
            Ok(_socket) => {
                println!("   ✓ IPC socket created successfully");
                println!("   (Would connect if server was running)\n");
            }
            Err(e) => {
                println!("   ✗ Expected connection error (no server): {}\n", e);
            }
        }
    }

    #[cfg(not(unix))]
    {
        println!("2. IPC Transport:");
        println!("   ⚠ IPC (Unix domain sockets) only available on Unix systems\n");
    }

    println!("3. Summary:");
    println!("   • TCP transport: tcp://host:port");
    println!("   • IPC transport: ipc:///path/to/socket.sock (Unix only)");
    println!("   • Both use the same socket types (DealerSocket, SubSocket, etc.)");
    println!("   • TCP: Use socket.connect(\"tcp://...\") or socket.connect(\"host:port\")");
    println!("   • IPC: Use socket.connect_ipc(\"/path\") - returns Socket<UnixStream>");
    println!("\n✓ Demo complete!");

    Ok(())
}
