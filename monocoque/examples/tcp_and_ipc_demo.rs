//! Demonstrates both TCP and IPC transport support.
//!
//! This example shows how to use both TCP and Unix domain sockets
//! with the same socket types. The transport is picked by the endpoint
//! scheme, and one socket can use several at once.
//!
//! Run with:
//! ```bash
//! cargo run --example tcp_and_ipc_demo --features zmq
//! ```

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, Result, RouterSocket};

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    println!("=== TCP and IPC Transport Demo ===\n");

    let mut router = RouterSocket::new();

    // TCP example
    println!("1. TCP Transport:");
    let tcp_endpoint = router.bind("tcp://127.0.0.1:0").await?;
    println!("   ROUTER bound to {tcp_endpoint}");
    let mut tcp_dealer = DealerSocket::new();
    tcp_dealer.connect(&tcp_endpoint).await?;
    tcp_dealer.send("hello over tcp").await?;
    let msg = router.recv().await?;
    println!(
        "   Received: {:?}\n",
        String::from_utf8_lossy(msg.get(1).unwrap_or_default())
    );

    // IPC example (Unix only)
    #[cfg(unix)]
    {
        println!("2. IPC Transport (Unix domain sockets):");

        // Clean up any existing socket file
        let socket_path = std::env::temp_dir().join("monocoque_tcp_and_ipc_demo.sock");
        let _ = std::fs::remove_file(&socket_path);

        // The same ROUTER now listens on both transports.
        let ipc_endpoint = router
            .bind(&format!("ipc://{}", socket_path.display()))
            .await?;
        println!("   ROUTER also bound to {ipc_endpoint}");
        let mut ipc_dealer = DealerSocket::new();
        ipc_dealer.connect(&ipc_endpoint).await?;
        ipc_dealer.send("hello over ipc").await?;
        let msg = router.recv().await?;
        println!(
            "   Received: {:?}\n",
            String::from_utf8_lossy(msg.get(1).unwrap_or_default())
        );
        println!("   ROUTER peers: {}\n", router.peer_count());
        let _ = std::fs::remove_file(&socket_path);
    }

    #[cfg(not(unix))]
    {
        println!("2. IPC Transport:");
        println!("   IPC (Unix domain sockets) only available on Unix systems\n");
    }

    println!("3. Summary:");
    println!("   - TCP transport: tcp://host:port");
    println!("   - IPC transport: ipc:///path/to/socket.sock (Unix only)");
    println!("   - Both use the same socket types (DealerSocket, SubSocket, etc.)");
    println!("   - socket.bind(..) and socket.connect(..) take any endpoint scheme");
    println!("   - One socket can bind and connect to any number of endpoints");
    println!("\nDemo complete!");

    Ok(())
}
