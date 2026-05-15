//! Demonstrates IPC transport using Unix domain sockets.
//!
//! This example shows how to use IPC for inter-process communication
//! with low latency and zero network overhead.
//!
//! Note: This example only works on Unix-like systems (Linux, macOS, BSD).

#[cfg(not(unix))]
fn main() {
    eprintln!("❌ IPC transport is only available on Unix-like systems");
    eprintln!("   This example requires Linux, macOS, or BSD");
    std::process::exit(1);
}

#[cfg(unix)]
#[compio::main]
async fn main() -> std::io::Result<()> {
    use compio::buf::BufResult;
    use compio::io::{AsyncRead, AsyncWriteExt};
    use monocoque::zmq::ipc;
    use std::time::Duration;

    let socket_path = "/tmp/monocoque_ipc_example.sock";

    println!("IPC Transport Example");
    println!("=====================\n");
    println!("Socket path: {}", socket_path);

    // Clean up any existing socket
    let _ = std::fs::remove_file(socket_path);

    // Create listener
    println!("\n1. Creating IPC listener...");
    let listener = ipc::bind(socket_path).await?;
    println!("   ✓ Listening on {}", socket_path);

    // Spawn server task
    let server_task = compio::runtime::spawn(async move {
        println!("   [Server] Waiting for connection...");

        match ipc::accept(&listener).await {
            Ok(mut stream) => {
                println!("   [Server] ✓ Client connected");

                // Receive message
                let mut buffer = vec![0u8; 1024];
                match stream.read(buffer).await {
                    BufResult(Ok(n), buffer) if n > 0 => {
                        let message = String::from_utf8_lossy(&buffer[..n]);
                        println!("   [Server] 📩 Received: {}", message);

                        // Send response
                        let response = "Hello from server!";
                        match stream.write_all(response.as_bytes()).await {
                            BufResult(Ok(_), _) => {
                                println!("   [Server] 📤 Sent response");
                            }
                            BufResult(Err(e), _) => {
                                eprintln!("   [Server] ❌ Write error: {}", e);
                            }
                        }
                    }
                    BufResult(Ok(_), _) => {
                        println!("   [Server] ⚠ Empty message received");
                    }
                    BufResult(Err(e), _) => {
                        eprintln!("   [Server] ❌ Read error: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("   [Server] ❌ Accept error: {}", e);
            }
        }
    });

    // Give server time to start
    compio::time::sleep(Duration::from_millis(50)).await;

    // Connect client
    println!("\n2. Connecting IPC client...");
    let mut client = ipc::connect(socket_path).await?;
    println!("   ✓ Connected to {}", socket_path);

    // Send message
    println!("\n3. Sending message from client...");
    let message = "Hello from client!";
    let BufResult(result, _) = client.write_all(message.as_bytes()).await;
    result?;
    println!("   ✓ Message sent");

    // Receive response
    println!("\n4. Waiting for server response...");
    let mut buffer = vec![0u8; 1024];
    let BufResult(result, buffer) = client.read(buffer).await;
    let n = result?;
    let response = String::from_utf8_lossy(&buffer[..n]);
    println!("   📩 Received: {}", response);

    // Wait for server to finish
    server_task.await;

    // Cleanup
    let _ = std::fs::remove_file(socket_path);

    println!("\n✅ IPC transport example completed successfully");
    println!("\nPerformance characteristics:");
    println!("  • Zero network overhead (in-kernel communication)");
    println!("  • Lower latency than TCP loopback");
    println!("  • No port allocation required");
    println!("  • Automatic cleanup on process exit");

    Ok(())
}
