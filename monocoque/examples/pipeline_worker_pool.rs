//! Pipeline (PUSH/PULL) worker pool example.
//!
//! Classic "divide and conquer" pattern:
//!
//! ```text
//! [Ventilator/PUSH] ──distributes tasks──▶ [Workers/PULL × N]
//!                                                  │
//! [Sink/PULL]       ◀──collects results── [Workers/PUSH × N]
//! ```
//!
//! Run with: `cargo run --example pipeline_worker_pool`

use bytes::Bytes;
use monocoque::zmq::{PullSocket, PushSocket};
use std::time::Instant;

const TASKS: usize = 100;
const WORKERS: usize = 4;

#[compio::main]
async fn main() -> std::io::Result<()> {
    // ── Ventilator: distributes tasks to workers ──────────────────────────
    let (_vent_listener, mut ventilator) = PushSocket::bind("127.0.0.1:5557").await?;

    // ── Sink: collects results from workers ───────────────────────────────
    let (_sink_listener, mut sink) = PullSocket::bind("127.0.0.1:5558").await?;

    // ── Workers: PULL from ventilator, PUSH results to sink ───────────────
    for i in 0..WORKERS {
        compio::runtime::spawn(async move {
            let mut work_rx = PullSocket::connect("127.0.0.1:5557").await.unwrap();
            let mut result_tx = PushSocket::connect("127.0.0.1:5558").await.unwrap();
            println!("Worker {} ready", i);
            while let Ok(Some(msg)) = work_rx.recv().await {
                let task = String::from_utf8_lossy(&msg[0]);
                let result = format!("worker-{} done: {}", i, task);
                result_tx.send(vec![Bytes::from(result)]).await.unwrap();
            }
        })
        .detach();
    }

    // Brief pause to let workers connect.
    compio::time::sleep(std::time::Duration::from_millis(50)).await;

    // ── Distribute tasks ──────────────────────────────────────────────────
    println!("Distributing {} tasks to {} workers…", TASKS, WORKERS);
    let start = Instant::now();
    for i in 0..TASKS {
        ventilator
            .send(vec![Bytes::from(format!("task-{}", i))])
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
