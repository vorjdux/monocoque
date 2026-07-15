//! Example demonstrating ROUTER identity assignment and management.
//!
//! Shows how to:
//! - Assign explicit identities to peers using `connect_routing_id`
//! - Route messages to specific workers by identity
//! - Use `router_mandatory` mode for error handling

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, RouterSocket};
use monocoque_core::options::SocketOptions;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ROUTER Identity Management Demo ===\n");

    // Start server in background
    rt::spawn_detached(async {
        run_server().await.expect("Server failed");
    });

    // Give server time to bind
    rt::sleep(Duration::from_millis(100)).await;

    // Create workers with custom identities
    println!("[Client] Creating workers with custom identities...");

    let mut worker1 = DealerSocket::connect_with_options(
        "127.0.0.1:5556",
        SocketOptions::default().with_routing_id(Bytes::from_static(b"worker-001")),
    )
    .await?;

    let mut worker2 = DealerSocket::connect_with_options(
        "127.0.0.1:5556",
        SocketOptions::default().with_routing_id(Bytes::from_static(b"worker-002")),
    )
    .await?;

    println!("[Client] Workers connected with identities:");
    println!("  - worker-001");
    println!("  - worker-002\n");

    // Workers send registration messages
    println!("[Worker 1] Sending registration...");
    worker1.send(vec![Bytes::from("READY")]).await?;

    println!("[Worker 2] Sending registration...");
    worker2.send(vec![Bytes::from("READY")]).await?;

    rt::sleep(Duration::from_millis(50)).await;

    // Workers receive and process tasks
    println!("\n[Worker 1] Waiting for tasks...");
    if let Ok(Some(msg)) = worker1.recv().await {
        println!(
            "[Worker 1] Received: {:?}",
            String::from_utf8_lossy(&msg[0])
        );
    }

    println!("[Worker 2] Waiting for tasks...");
    if let Ok(Some(msg)) = worker2.recv().await {
        println!(
            "[Worker 2] Received: {:?}",
            String::from_utf8_lossy(&msg[0])
        );
    }

    // Cleanup
    rt::sleep(Duration::from_millis(100)).await;

    println!("\n=== Demo Complete ===");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    // Bind ROUTER socket; bind() accepts the first connection (worker 1).
    let (listener, mut router) = RouterSocket::bind("127.0.0.1:5556").await?;
    println!("[Server] ROUTER bound to 127.0.0.1:5556");

    // Enable router_mandatory for error handling
    router.set_router_mandatory(true);
    println!("[Server] ROUTER_MANDATORY enabled");

    // Accept the second connection (worker 2) up front. The handshake is eager,
    // so a connecting DEALER does not return until the ROUTER has accepted it;
    // accepting both peers before receiving keeps the two clients from blocking.
    let (stream2, _) = listener.accept().await?;
    let mut router2 = RouterSocket::from_tcp(stream2).await?;

    println!(
        "[Server] First peer identity:  {:?}",
        router.peer_identity()
    );
    println!(
        "[Server] Second peer identity: {:?}\n",
        router2.peer_identity()
    );

    // Receive registration from the first worker. A DEALER->ROUTER message is
    // [identity, ...payload] with no empty delimiter (that is a REQ/REP thing).
    if let Ok(Some(msg)) = router.recv().await {
        println!(
            "[Server] Registration from {:?}: {:?}",
            String::from_utf8_lossy(&msg[0]),
            String::from_utf8_lossy(&msg[1])
        );
    }

    // Receive registration from the second worker.
    if let Ok(Some(msg)) = router2.recv().await {
        println!(
            "[Server] Registration from {:?}: {:?}",
            String::from_utf8_lossy(&msg[0]),
            String::from_utf8_lossy(&msg[1])
        );
    }

    // Route tasks to specific workers
    println!("\n[Server] Routing tasks to workers...");

    // Task for worker 1
    println!("[Server] Sending task to {:?}", router.peer_identity());
    router
        .send(vec![
            router.peer_identity().clone(),
            Bytes::from("Task: Process dataset A"),
        ])
        .await?;

    // Task for worker 2
    println!("[Server] Sending task to {:?}", router2.peer_identity());
    router2
        .send(vec![
            router2.peer_identity().clone(),
            Bytes::from("Task: Process dataset B"),
        ])
        .await?;

    println!("[Server] Tasks dispatched\n");

    // Keep server alive briefly
    rt::sleep(Duration::from_millis(500)).await;

    Ok(())
}
