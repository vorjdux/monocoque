//! ROUTER Worker Pool Example
//!
//! This example demonstrates a ROUTER socket acting as a load balancer
//! distributing work across multiple DEALER workers.
//!
//! Architecture:
//! - ROUTER server binds an ephemeral TCP port
//! - Several DEALER workers connect and announce themselves with "READY"
//! - The ROUTER hands out tasks round-robin by identity and collects results
//!
//! Each worker names itself with `with_routing_id`, so the identity frame the
//! ROUTER sees is readable. Without it the ROUTER makes up a binary identity
//! for each connection.

use monocoque::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, Msg, Result, RouterSocket, SocketOptions};
use tracing::info;

const WORKERS: usize = 3;
const TASKS: usize = 10;
const STOP: &str = "STOP";

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    info!("Starting ROUTER worker pool on {endpoint}");

    let handles: Vec<_> = (0..WORKERS)
        .map(|i| rt::spawn(worker(i, endpoint.clone())))
        .collect();

    // Collect one READY per worker to learn their identities.
    info!("Waiting for worker connections...");
    let mut workers = Vec::with_capacity(WORKERS);
    while workers.len() < WORKERS {
        // Only the identity frame is kept, so the message stays borrowed.
        let msg = router.recv().await?;
        if msg.get(1) == Some(&b"READY"[..])
            && let Some(id) = msg.frame_bytes(0)
        {
            info!("Worker {:?} ready", String::from_utf8_lossy(&id));
            workers.push(id);
        }
    }

    // Send tasks round-robin, then collect every result.
    for task_id in 0..TASKS {
        let id = workers[task_id % WORKERS].clone();
        let task = format!("Task #{task_id}");
        info!("Sending {task} to {:?}", String::from_utf8_lossy(&id));
        router.send(Msg::from([id, Bytes::from(task)])).await?;
    }
    for _ in 0..TASKS {
        let response = router.recv().await?;
        if let Some(result) = response.last()
            && let Ok(s) = std::str::from_utf8(result)
        {
            info!("Worker completed: {s}");
        }
    }

    // A ROUTER addresses each worker by identity, so every one gets its own
    // stop message. Joining the tasks brings any worker error back here.
    for id in workers {
        router
            .send(Msg::from([id, Bytes::from_static(STOP.as_bytes())]))
            .await?;
    }
    for handle in handles {
        rt::join(handle).await?;
    }
    router.close().await?;

    info!("Worker pool session complete");
    Ok(())
}

async fn worker(n: usize, endpoint: String) -> Result<()> {
    let identity = Bytes::from(format!("worker-{n}"));
    let mut socket = DealerSocket::with_options(SocketOptions::default().with_routing_id(identity));
    socket.connect(&endpoint).await?;
    socket.send("READY").await?;
    loop {
        // The task is only read, so it stays borrowed from the socket until
        // the result is built.
        let result = {
            let task = socket.recv().await?;
            let task = task.first().unwrap_or_default();
            if task == STOP.as_bytes() {
                break;
            }
            format!("{} done by worker {n}", String::from_utf8_lossy(task))
        };
        socket.send(result).await?;
    }
    socket.close().await
}
