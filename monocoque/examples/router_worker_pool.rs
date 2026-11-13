//! ROUTER Worker Pool Example
//!
//! This example demonstrates a ROUTER socket acting as a load balancer
//! distributing work across multiple DEALER workers.
//!
//! Architecture:
//! - ROUTER server binds an ephemeral TCP port
//! - Several DEALER workers connect and announce themselves with "READY"
//! - The ROUTER hands out tasks round-robin by identity and collects results

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, Msg, Result, RouterSocket};
use tracing::info;

const WORKERS: usize = 3;
const TASKS: usize = 10;

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

    for i in 0..WORKERS {
        let endpoint = endpoint.clone();
        rt::spawn_detached(async move {
            if let Err(e) = worker(i, &endpoint).await {
                info!("Worker {i} stopped: {e}");
            }
        });
    }

    // Collect one READY per worker to learn their identities.
    info!("Waiting for worker connections...");
    let mut workers = Vec::with_capacity(WORKERS);
    while workers.len() < WORKERS {
        let msg = router.recv_owned().await?;
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

    info!("Worker pool session complete");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn worker(n: usize, endpoint: &str) -> Result<()> {
    let mut socket = DealerSocket::new();
    socket.connect(endpoint).await?;
    socket.send("READY").await?;
    loop {
        let task = socket.recv_owned().await?;
        let text = String::from_utf8_lossy(task.first().unwrap_or_default()).into_owned();
        socket.send(format!("{text} done by worker {n}")).await?;
    }
}
