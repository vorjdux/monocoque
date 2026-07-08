//! Allocation-free receive with `recv_into` / `try_recv_into`.
//!
//! `recv()` allocates a fresh `Vec<Bytes>` for every message. At small message
//! sizes that allocation is the dominant per-message cost. `recv_into(&mut buf)`
//! writes the frames into a buffer you own, so reusing one buffer across the loop
//! allocates nothing per message; `try_recv_into` drains the rest of a kernel
//! read the same way.
//!
//! Run with: `cargo run --example recv_into_zero_alloc --features zmq`

use bytes::Bytes;
use monocoque::SocketOptions;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{PullSocket, PushSocket};
use std::time::Instant;

const ADDR: &str = "127.0.0.1:5601";
const MESSAGES: usize = 200_000;

fn main() -> std::io::Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

#[allow(clippy::cast_precision_loss)]
async fn async_main() -> std::io::Result<()> {
    // Sender: connect once the listener below is bound, push MESSAGES, then flush.
    // Spawned before `bind`, but on a single-threaded runtime it does not run
    // until the first `.await`, by which point the port is already bound.
    rt::spawn_detached(async move {
        let mut push = PushSocket::connect_with_options(
            ADDR,
            SocketOptions::default().with_write_coalescing(true),
        )
        .await
        .unwrap();
        let payload = Bytes::from_static(b"task");
        for _ in 0..MESSAGES {
            push.send(vec![payload.clone()]).await.unwrap();
        }
        push.flush().await.unwrap();
    });

    // Receiver: accept the connection, then drain with a single reused buffer.
    let (_listener, mut pull) = PullSocket::bind(ADDR).await?;

    let mut buf: Vec<Bytes> = Vec::with_capacity(4);
    let mut count = 0usize;
    let start = Instant::now();
    while count < MESSAGES {
        // Blocks for the next message, written into `buf` (no allocation).
        if !pull.recv_into(&mut buf).await? {
            break; // connection closed
        }
        count += 1;
        // Drain everything else already decoded from the same kernel read,
        // still reusing `buf` and still allocating nothing.
        while pull.try_recv_into(&mut buf)? {
            count += 1;
        }
    }
    let elapsed = start.elapsed();

    println!(
        "received {count} messages into one reused buffer in {:.1} ms ({:.2} M msg/s), \
         zero per-message allocation",
        elapsed.as_secs_f64() * 1000.0,
        count as f64 / elapsed.as_secs_f64() / 1e6,
    );
    Ok(())
}
