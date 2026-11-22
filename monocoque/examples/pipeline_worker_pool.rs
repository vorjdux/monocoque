//! Pipeline (PUSH/PULL) worker pool with write coalescing.
//!
//! The "divide and conquer" pattern:
//!
//! ```text
//! [Ventilator / PUSH] --tasks--> [Workers / PULL x N]
//!                                        |
//! [Sink / PULL]     <--results-- [Workers / PUSH x N]
//!
//! [Control / PUB]   --stop-----> [Workers / SUB x N]
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
//! Every queue in the loop is bounded, so the sink has to be read while the
//! ventilator is still sending. Here the sink runs as its own task.
//!
//! Run with: `cargo run --release --example pipeline_worker_pool --features zmq`
//!
//! An optional argument sets the number of tasks (default 100000):
//! `cargo run --release --example pipeline_worker_pool --features zmq -- 2000000`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::poll::{PollEvents, PollItem, poll};
use monocoque::zmq::{PubSocket, PullSocket, PushSocket, Result, SocketOptions, SubSocket};
use std::time::{Duration, Instant};

const TASKS: usize = 100_000;
const WORKERS: usize = 4;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let tasks = match std::env::args().nth(1) {
        Some(arg) => arg.parse()?,
        None => TASKS,
    };
    LocalRuntime::new()?.block_on(async_main(tasks))?;
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
async fn async_main(tasks: usize) -> Result<()> {
    let coalescing = SocketOptions::default().with_write_coalescing(true);

    let mut ventilator = PushSocket::with_options(coalescing);
    let vent_endpoint = ventilator.bind("tcp://127.0.0.1:0").await?;
    let mut sink = PullSocket::new();
    let sink_endpoint = sink.bind("tcp://127.0.0.1:0").await?;
    let mut control = PubSocket::new();
    let control_endpoint = control.bind("tcp://127.0.0.1:0").await?;

    let workers: Vec<_> = (0..WORKERS)
        .map(|id| {
            rt::spawn(worker(
                id,
                vent_endpoint.clone(),
                sink_endpoint.clone(),
                control_endpoint.clone(),
            ))
        })
        .collect();

    // PUSH only balances across peers that are already connected, and a PUB
    // drops what nobody has subscribed to yet, so wait for the whole pool and
    // its control subscriptions before handing out work.
    while ventilator.peer_count() < WORKERS
        || sink.peer_count() < WORKERS
        || subscribed(&control) < WORKERS
    {
        rt::sleep(Duration::from_millis(5)).await;
    }

    println!("Distributing {tasks} tasks to {WORKERS} workers...");
    let start = Instant::now();

    // The sink is read while the tasks are still being sent. Sending them all
    // first and reading afterwards deadlocks once there are more tasks than
    // the queues and kernel buffers hold: results back up, the workers stop
    // taking tasks, and the ventilator's `send` waits for room that only
    // reading the sink can make.
    let collecting = rt::spawn(collect(sink, tasks));

    for i in 0..tasks {
        ventilator.send(format!("task-{i}")).await?;
    }
    // The burst is over: push out whatever is still buffered.
    ventilator.flush().await?;

    let (per_worker, finished) = rt::join(collecting).await?;
    let elapsed = finished.duration_since(start);

    println!(
        "All {tasks} tasks completed in {:.1} ms ({:.0} tasks/sec)",
        elapsed.as_secs_f64() * 1000.0,
        tasks as f64 / elapsed.as_secs_f64()
    );
    println!("Tasks per worker: {per_worker:?}");

    // A receive does not fail when the peer goes away, so the workers are told
    // to stop. A stop sent down the task socket would reach whichever workers
    // round-robin picked, which is one each only while every worker is idle
    // and has room. A PUB reaches every subscriber whatever it is doing, and
    // each worker polls its control socket next to its task socket.
    control.send("stop").await?;
    for handle in workers {
        rt::join(handle).await?;
    }
    ventilator.close().await?;
    control.close().await
}

/// How many workers have subscribed to the control socket.
fn subscribed(control: &PubSocket) -> usize {
    control
        .peer_ids()
        .into_iter()
        .filter(|&peer| control.subscriber_prefixes(peer) > 0)
        .count()
}

/// Read `tasks` results from the sink. Returns how many each worker did and
/// when the last one arrived.
async fn collect(mut sink: PullSocket, tasks: usize) -> Result<([usize; WORKERS], Instant)> {
    let mut per_worker = [0usize; WORKERS];
    for _ in 0..tasks {
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
    let finished = Instant::now();
    sink.close().await?;
    Ok((per_worker, finished))
}

/// PULL tasks from the ventilator and PUSH one result per task to the sink,
/// until the control socket says to stop.
async fn worker(
    id: usize,
    vent_endpoint: String,
    sink_endpoint: String,
    control_endpoint: String,
) -> Result<()> {
    let mut tasks = PullSocket::new();
    tasks.connect(&vent_endpoint).await?;
    let mut results =
        PushSocket::with_options(SocketOptions::default().with_write_coalescing(true));
    results.connect(&sink_endpoint).await?;
    let mut control = SubSocket::new();
    control.connect(&control_endpoint).await?;
    control.subscribe("").await?;

    loop {
        while tasks.has_message() {
            // The task is only read, so it stays borrowed from the socket
            // until the result is built.
            let result = {
                let task = tasks.recv().await?;
                let task = task.first().unwrap_or_default();
                format!("worker-{id} done: {}", String::from_utf8_lossy(task))
            };
            results.send(result).await?;
        }
        // A worker cannot see the end of the ventilator's burst, but it can
        // see its own queue run dry. Flushing then keeps results batched
        // while tasks keep arriving and still delivers the last one promptly.
        results.flush().await?;

        // Wait for the next task or for the stop command, whichever is first.
        let stop = {
            let mut items = [
                PollItem::new(&tasks, PollEvents::POLLIN),
                PollItem::new(&control, PollEvents::POLLIN),
            ];
            poll(&mut items, None).await?;
            items[1].is_readable()
        };
        if stop {
            break;
        }
    }
    results.close().await
}
