//! Example demonstrating ROUTER identity assignment and management.
//!
//! Shows how to:
//! - Assign explicit identities to peers using `with_routing_id`
//! - Route messages to specific workers by identity
//! - Use `router_mandatory` mode for error handling

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, MonocoqueError, Msg, Result, RouterSocket, SocketOptions};

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    println!("=== ROUTER Identity Management Demo ===\n");

    // Bind ROUTER socket with router_mandatory, so sending to an unknown
    // identity is an error instead of a silent drop.
    let mut router =
        RouterSocket::with_options(SocketOptions::default().with_router_mandatory(true));
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    println!("[Server] ROUTER bound to {endpoint}");
    println!("[Server] ROUTER_MANDATORY enabled");

    let server = rt::spawn(async move { run_server(router).await });

    // Create workers with custom identities
    println!("[Client] Creating workers with custom identities...");

    let mut worker1 = DealerSocket::with_options(
        SocketOptions::default().with_routing_id(Bytes::from_static(b"worker-001")),
    );
    worker1.connect(&endpoint).await?;

    let mut worker2 = DealerSocket::with_options(
        SocketOptions::default().with_routing_id(Bytes::from_static(b"worker-002")),
    );
    worker2.connect(&endpoint).await?;

    println!("[Client] Workers connecting with identities:");
    println!("  - worker-001");
    println!("  - worker-002\n");

    // Workers send registration messages
    println!("[Worker 1] Sending registration...");
    worker1.send("READY").await?;

    println!("[Worker 2] Sending registration...");
    worker2.send("READY").await?;

    // Workers receive and process tasks
    println!("\n[Worker 1] Waiting for tasks...");
    let msg = worker1.recv().await?;
    println!(
        "[Worker 1] Received: {:?}",
        String::from_utf8_lossy(msg.first().unwrap_or_default())
    );

    println!("[Worker 2] Waiting for tasks...");
    let msg = worker2.recv().await?;
    println!(
        "[Worker 2] Received: {:?}",
        String::from_utf8_lossy(msg.first().unwrap_or_default())
    );

    rt::join(server).await?;
    println!("\n=== Demo Complete ===");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn run_server(mut router: RouterSocket) -> Result<()> {
    // Receive both registrations. A DEALER->ROUTER message is
    // [identity, ...payload] with no empty delimiter (that is a REQ/REP thing).
    for _ in 0..2 {
        let msg = router.recv().await?;
        println!(
            "[Server] Registration from {:?}: {:?}",
            String::from_utf8_lossy(msg.get(0).unwrap_or_default()),
            String::from_utf8_lossy(msg.get(1).unwrap_or_default())
        );
    }

    let mut known: Vec<String> = router
        .identities()
        .iter()
        .map(|id| String::from_utf8_lossy(id).into_owned())
        .collect();
    known.sort();
    println!("[Server] Connected identities: {known:?}");

    // Route tasks to specific workers
    println!("\n[Server] Routing tasks to workers...");
    for (identity, task) in [
        (&b"worker-001"[..], "Task: Process dataset A"),
        (&b"worker-002"[..], "Task: Process dataset B"),
    ] {
        println!(
            "[Server] Sending task to {:?}",
            String::from_utf8_lossy(identity)
        );
        router
            .send(Msg::from([
                Bytes::copy_from_slice(identity),
                Bytes::from(task),
            ]))
            .await?;
    }

    // With router_mandatory, an unknown identity is reported back.
    match router
        .send(Msg::from([
            Bytes::from_static(b"worker-999"),
            Bytes::from("Task: nobody home"),
        ]))
        .await
    {
        Err(MonocoqueError::HostUnreachable) => {
            println!("[Server] worker-999 is unknown: HostUnreachable (as expected)");
        }
        other => println!("[Server] Unexpected result for worker-999: {other:?}"),
    }

    println!("[Server] Tasks dispatched\n");
    router.close().await
}
