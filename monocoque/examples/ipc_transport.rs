//! Demonstrates IPC transport using Unix domain sockets.
//!
//! This example shows how to use IPC for inter-process communication
//! with low latency and zero network overhead. Any socket binds or connects
//! to an `ipc://` endpoint exactly as it would to a `tcp://` one.
//!
//! Note: This example only works on Unix-like systems (Linux, macOS, BSD).

#[cfg(not(unix))]
fn main() {
    eprintln!("IPC transport is only available on Unix-like systems");
    eprintln!("This example requires Linux, macOS, or BSD");
    std::process::exit(1);
}

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    monocoque::rt::LocalRuntime::new()?.block_on(async_main())
}

#[cfg(unix)]
async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    use monocoque::rt;
    use monocoque::zmq::{RepSocket, ReqSocket};

    let socket_path = "/tmp/monocoque_ipc_example.sock";
    let endpoint = format!("ipc://{socket_path}");

    println!("IPC Transport Example");
    println!("=====================\n");
    println!("Endpoint: {endpoint}");

    // Clean up any existing socket
    let _ = std::fs::remove_file(socket_path);

    // Bind the server
    println!("\n1. Binding REP socket...");
    let mut server = RepSocket::new();
    server.bind(&endpoint).await?;
    println!("   Listening on {endpoint}");

    // Spawn server task
    let server_task = rt::spawn(async move {
        println!("   [Server] Waiting for a request...");
        match server.recv().await {
            Ok(request) => {
                let message = String::from_utf8_lossy(request.first().unwrap_or_default());
                println!("   [Server] Received: {message}");
            }
            Err(e) => {
                eprintln!("   [Server] Receive error: {e}");
                return;
            }
        }
        match server.send("Hello from server!").await {
            Ok(()) => println!("   [Server] Sent response"),
            Err(e) => eprintln!("   [Server] Send error: {e}"),
        }
        let _ = server.close().await;
    });

    // Connect client
    println!("\n2. Connecting REQ client...");
    let mut client = ReqSocket::new();
    client.connect(&endpoint).await?;
    println!("   Connecting to {endpoint}");

    // Send message
    println!("\n3. Sending message from client...");
    client.send("Hello from client!").await?;
    println!("   Message sent");

    // Receive response
    println!("\n4. Waiting for server response...");
    {
        let response = client.recv().await?;
        println!(
            "   Received: {}",
            String::from_utf8_lossy(response.first().unwrap_or_default())
        );
    }

    // Wait for server to finish
    rt::join(server_task).await;

    // Cleanup
    let _ = std::fs::remove_file(socket_path);

    println!("\nIPC transport example completed successfully");
    println!("\nPerformance characteristics:");
    println!("  - Zero network overhead (in-kernel communication)");
    println!("  - Lower latency than TCP loopback");
    println!("  - No port allocation required");

    Ok(())
}
