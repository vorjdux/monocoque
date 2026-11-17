//! Pipeline (PUSH/PULL) worker pool with write coalescing.
//!
//! The "divide and conquer" pattern:
//!
//! ```text
//! [Ventilator / PUSH] --tasks--> [Workers / PULL x N]
//!                                        |
//! [Sink / PULL]     <--results-- [Workers / PUSH x N]
//! ```
//!
//! A bound PUSH socket round-robins tasks across every connected worker and a
//! bound PULL socket fair-queues every worker's results into one stream.
//!
//! The tasks are small and sent in a burst, which is the case write coalescing
//! is for: sends accumulate in a buffer and reach the kernel in a few large
//! writes instead of one write per message. The price is that the sender owns
//! the flush: nothing below the coalescing threshold leaves until `flush()`.
//!
//! Run with: `cargo run --example pipeline_worker_pool --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{PullSocket, PushSocket, Result, SocketOptions};
use std::time::{Duration, Instant};

const TASKS: usize = 100_000;
const WORKERS: usize = 4;
const STOP: &str = "stop";

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let coalescing = SocketOptions::default().with_write_coalescing(true);

    let mut ventilator = PushSocket::with_options(coalescing);
    let vent_endpoint = ventilator.bind("tcp://127.0.0.1:0").await?;
    let mut sink = PullSocket::new();
    let sink_endpoint = sink.bind("tcp://127.0.0.1:0").await?;

    let workers: Vec<_> = (0..WORKERS)
        .map(|id| rt::spawn(worker(id, vent_endpoint.clone(), sink_endpoint.clone())))
        .collect();

    // PUSH only balances across peers that are already connected, so wait for
    // the whole pool before handing out work.
    while ventilator.peer_count() < WORKERS || sink.peer_count() < WORKERS {
        rt::sleep(Duration::from_millis(5)).await;
    }

    println!("Distributing {TASKS} tasks to {WORKERS} workers...");
    let start = Instant::now();
    for i in 0..TASKS {
        ventilator.send(format!("task-{i}")).await?;
    }
    // The burst is over: push out whatever is still buffered.
    ventilator.flush().await?;

    let mut per_worker = [0usize; WORKERS];
    for _ in 0..TASKS {
        // The result is only read, so it stays borrowed from the socket.
        let result = sink.recv().await?;
        let text = String::from_utf8_lossy(result.first().unwrap_or_default());
        if let Some(count) = text
            .strip_prefix("worker-")
            .and_then(|rest| rest.split(' ').next())
            .and_then(|id| id.parse::<usize>().ok())
            .and_then(|id| per_worker.get_mut(id))
        {
            *count += 1;
        }
    }
    let elapsed = start.elapsed();

    println!(
        "All {TASKS} tasks completed in {:.1} ms ({:.0} tasks/sec)",
        elapsed.as_secs_f64() * 1000.0,
        f64::from(u32::try_from(TASKS).unwrap_or(u32::MAX)) / elapsed.as_secs_f64()
    );
    println!("Tasks per worker: {per_worker:?}");

    // A receive does not fail when the peer goes away, so the workers are told
    // to stop. With every worker idle, round-robin hands each one message.
    for _ in 0..WORKERS {
        ventilator.send(STOP).await?;
    }
    ventilator.close().await?;
    for handle in workers {
        rt::join(handle).await?;
    }
    sink.close().await
}

/// PULL tasks from the ventilator and PUSH one result per task to the sink.
async fn worker(id: usize, vent_endpoint: String, sink_endpoint: String) -> Result<()> {
    let mut tasks = PullSocket::new();
    tasks.connect(&vent_endpoint).await?;
    let mut results =
        PushSocket::with_options(SocketOptions::default().with_write_coalescing(true));
    results.connect(&sink_endpoint).await?;

    loop {
        // The task is only read, so it stays borrowed from the socket until
        // the result is built.
        let result = {
            let task = tasks.recv().await?;
            let task = task.first().unwrap_or_default();
            if task == STOP.as_bytes() {
                break;
            }
            format!("worker-{id} done: {}", String::from_utf8_lossy(task))
        };
        results.send(result).await?;
        // A worker cannot see the end of the ventilator's burst, but it can
        // see its own queue run dry. Flushing then keeps results batched
        // while tasks keep arriving and still delivers the last one promptly.
        if !tasks.has_message() {
            results.flush().await?;
        }
    }
    results.close().await
}
