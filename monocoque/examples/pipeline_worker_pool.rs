//! Pipeline (PUSH/PULL) worker pool example.
//!
//! Classic "divide and conquer" pattern:
//!
//! ```text
//! [Ventilator / PUSH] ──distributes tasks──▶ [Workers / PULL × N]
//!                                                    │
//! [Sink / PULL]       ◀──collects results── [Workers / PUSH × N]
//! ```
//!
//! A bound PUSH socket round-robins tasks across every connected worker and a
//! bound PULL socket fair-queues every worker's results into one stream.
//!
//! Run with: `cargo run --example pipeline_worker_pool --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{PullSocket, PushSocket, Result};
use std::time::{Duration, Instant};

const TASKS: usize = 100;
const WORKERS: usize = 4;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

#[allow(clippy::cast_precision_loss)]
async fn async_main() -> Result<()> {
    let mut ventilator = PushSocket::new();
    let vent_endpoint = ventilator.bind("tcp://127.0.0.1:0").await?;
    let mut sink = PullSocket::new();
    let sink_endpoint = sink.bind("tcp://127.0.0.1:0").await?;

    // ── Workers: PULL from the ventilator, PUSH results to the sink ───────
    for i in 0..WORKERS {
        let vent_endpoint = vent_endpoint.clone();
        let sink_endpoint = sink_endpoint.clone();
        rt::spawn_detached(async move {
            let mut work_rx = PullSocket::new();
            work_rx.connect(&vent_endpoint).await.unwrap();
            let mut result_tx = PushSocket::new();
            result_tx.connect(&sink_endpoint).await.unwrap();
            println!("Worker {i} ready");
            while let Ok(msg) = work_rx.recv().await {
                let task = String::from_utf8_lossy(msg.first().unwrap_or_default());
                let result = format!("worker-{i} done: {task}");
                result_tx.send(result).await.unwrap();
            }
        });
    }

    // PUSH only balances across peers that are already connected, so wait for
    // the whole pool before handing out work.
    while ventilator.peer_count() < WORKERS {
        rt::sleep(Duration::from_millis(5)).await;
    }

    // ── Distribute tasks ──────────────────────────────────────────────────
    println!("Distributing {TASKS} tasks to {WORKERS} workers...");
    let start = Instant::now();
    for i in 0..TASKS {
        ventilator.send(format!("task-{i}")).await?;
    }

    // ── Collect results ───────────────────────────────────────────────────
    let mut per_worker = [0usize; WORKERS];
    for _ in 0..TASKS {
        let result = sink.recv().await?;
        let text = String::from_utf8_lossy(result.first().unwrap_or_default());
        if let Some(n) = text
            .strip_prefix("worker-")
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse::<usize>().ok())
        {
            per_worker[n] += 1;
        }
    }

    let elapsed = start.elapsed();
    println!(
        "All {} tasks completed in {:.2}ms ({:.0} tasks/sec)",
        TASKS,
        elapsed.as_secs_f64() * 1000.0,
        TASKS as f64 / elapsed.as_secs_f64()
    );
    println!("Tasks per worker: {per_worker:?}");

    Ok(())
}
