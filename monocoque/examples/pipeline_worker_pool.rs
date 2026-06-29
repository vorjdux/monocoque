//! Pipeline (PUSH/PULL) worker pool example.
//!
//! Classic "divide and conquer" pattern:
//!
//! ```text
//! [Ventilator / PushFanOut] ──distributes tasks──▶ [Workers / PULL × N]
//!                                                          │
//! [Sink / PullFanIn]        ◀──collects results── [Workers / PUSH × N]
//! ```
//!
//! The ventilator round-robins tasks across the whole worker pool and the sink
//! merges every worker's results into one stream, so this scales past a single
//! worker (a plain `PushSocket`/`PullSocket` pair only ever talks to one peer).
//!
//! Run with: `cargo run --example pipeline_worker_pool`

use bytes::Bytes;
use compio::net::TcpListener;
use monocoque::zmq::{PullFanIn, PullSocket, PushFanOut, PushSocket};
use std::time::Instant;

const TASKS: usize = 100;
const WORKERS: usize = 4;

#[compio::main]
#[allow(clippy::cast_precision_loss)]
async fn main() -> std::io::Result<()> {
    // Bind both listeners up front so the workers can connect to either one
    // before we start accepting the pool.
    let vent_listener = TcpListener::bind("127.0.0.1:5557").await?;
    let sink_listener = TcpListener::bind("127.0.0.1:5558").await?;

    // ── Workers: PULL from the ventilator, PUSH results to the sink ───────
    for i in 0..WORKERS {
        compio::runtime::spawn(async move {
            let mut work_rx = PullSocket::connect("127.0.0.1:5557").await.unwrap();
            let mut result_tx = PushSocket::connect("127.0.0.1:5558").await.unwrap();
            println!("Worker {i} ready");
            while let Ok(Some(msg)) = work_rx.recv().await {
                let task = String::from_utf8_lossy(&msg[0]);
                let result = format!("worker-{i} done: {task}");
                result_tx.send(vec![Bytes::from(result)]).await.unwrap();
            }
        })
        .detach();
    }

    // ── Accept the pool on both ends ──────────────────────────────────────
    let mut ventilator =
        PushFanOut::accept_workers(&vent_listener, WORKERS, monocoque::SocketOptions::default())
            .await?;
    let mut sink =
        PullFanIn::accept_workers(&sink_listener, WORKERS, monocoque::SocketOptions::default())
            .await?;

    // ── Distribute tasks ──────────────────────────────────────────────────
    println!("Distributing {TASKS} tasks to {WORKERS} workers…");
    let start = Instant::now();
    for i in 0..TASKS {
        ventilator
            .send(vec![Bytes::from(format!("task-{i}"))])
            .await?;
    }

    // ── Collect results ───────────────────────────────────────────────────
    for _ in 0..TASKS {
        if let Ok(Some(result)) = sink.recv().await {
            let _ = String::from_utf8_lossy(&result[0]); // process result
        }
    }

    let elapsed = start.elapsed();
    println!(
        "All {} tasks completed in {:.2}ms ({:.0} tasks/sec)",
        TASKS,
        elapsed.as_millis(),
        TASKS as f64 / elapsed.as_secs_f64()
    );

    Ok(())
}
