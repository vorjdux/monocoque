//! SCATTER/GATHER: a pipeline of single-part messages (draft).
//!
//! SCATTER and GATHER are the draft counterparts of PUSH and PULL. A SCATTER
//! round-robins its messages across the GATHER peers connected to it, and a
//! GATHER fair-queues what its SCATTER peers send. The difference is that the
//! messages are single-part: sending more than one frame is an error, and a
//! multipart message from a peer is dropped.
//!
//! Use them where PUSH/PULL would do and both ends are draft-aware. A draft
//! socket only talks to its own partner type, so a SCATTER cannot feed a
//! PULL.
//!
//! The draft socket types follow libzmq's draft API, which may still change,
//! so they sit behind the `draft` feature.
//!
//! Run with: `cargo run --example draft_scatter_gather --features zmq,draft`

use monocoque::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::draft::{GatherSocket, ScatterSocket};
use monocoque::zmq::{MonocoqueError, Msg, Result};
use std::time::Duration;

const WORKERS: usize = 2;
const TASKS: usize = 6;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut scatter = ScatterSocket::new();
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = scatter.bind("tcp://127.0.0.1:0").await?;
    println!("[SCATTER] listening on {endpoint}");

    let mut workers = Vec::new();
    for _ in 0..WORKERS {
        let mut gather = GatherSocket::new();
        gather.connect(&endpoint).await?;
        workers.push(gather);
    }
    // A SCATTER only balances across peers that are already connected, so
    // wait for both before handing out work.
    while scatter.peer_count() < WORKERS {
        rt::sleep(Duration::from_millis(1)).await;
    }

    for task in 1..=TASKS {
        scatter.send(format!("task {task}")).await?;
    }
    println!("[SCATTER] sent {TASKS} tasks");

    // Round-robin gave each worker every other task.
    for (n, gather) in workers.iter_mut().enumerate() {
        for _ in 0..TASKS / WORKERS {
            // The task is only printed, so it stays borrowed from the socket.
            let task = gather.recv().await?;
            println!(
                "[GATHER {n}] {}",
                String::from_utf8_lossy(task.first().unwrap_or_default())
            );
        }
    }

    // Single-part only: a two-frame message is refused before it is sent.
    let two_frames = Msg::from(["two", "frames"].map(Bytes::from));
    match scatter.send(two_frames).await {
        Err(MonocoqueError::InvalidState(why)) => println!("[SCATTER] two frames refused: {why}"),
        other => println!("[SCATTER] unexpected: {other:?}"),
    }

    scatter.close().await?;
    for gather in workers {
        gather.close().await?;
    }
    Ok(())
}
