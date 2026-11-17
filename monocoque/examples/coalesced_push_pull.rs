//! PUSH/PULL throughput the recommended way.
//!
//! Three things make a pipeline of small messages fast:
//!
//! - write coalescing on the sender: sends accumulate in a buffer and reach
//!   the kernel in a few large writes, with `flush()` after the burst;
//! - borrowed `recv()` on the receiver: each message is read in place and
//!   nothing is allocated per message;
//! - one thread per side. A socket has no background I/O thread: it does its
//!   work on the runtime thread that owns it. Here the sender and the
//!   receiver each get a thread with its own `LocalRuntime`.
//!
//! Use this shape for a stream of small messages where throughput matters
//! more than the latency of a single message. Leave coalescing off when each
//! message should leave as soon as it is sent.
//!
//! Run with: `cargo run --release --example coalesced_push_pull --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// The error crosses a thread boundary, so it has to be `Send`.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const MESSAGES: usize = 1_000_000;
const PAYLOAD: &[u8; 64] = &[7; 64];

fn main() -> Result<()> {
    let (endpoint_tx, endpoint_rx) = mpsc::channel();
    let receiver = thread::spawn(move || receive(&endpoint_tx));

    // The receiver reports the endpoint it bound. If it failed before that,
    // its end of the channel is gone and the join says why.
    let started = match endpoint_rx.recv() {
        Ok(endpoint) => LocalRuntime::new()?.block_on(send(endpoint))?,
        Err(_) => Instant::now(),
    };
    let (bytes, finished) = receiver
        .join()
        .map_err(|_| "the receiver thread panicked")??;

    println!(
        "{MESSAGES} messages ({bytes} bytes) sent and received in {:.1} ms",
        finished.duration_since(started).as_secs_f64() * 1000.0
    );
    Ok(())
}

/// The sending side. Returns when the burst started.
async fn send(endpoint: String) -> Result<Instant> {
    let mut push = PushSocket::with_options(SocketOptions::default().with_write_coalescing(true));
    push.connect(&endpoint).await?;
    // `connect` dials in the background. Start the clock on a live
    // connection so the time below is the transfer and not the dial.
    while push.peer_count() == 0 {
        rt::sleep(Duration::from_millis(1)).await;
    }

    let started = Instant::now();
    for _ in 0..MESSAGES {
        // A `&'static` payload is shared, not copied. When the buffer
        // reaches the coalescing threshold it is written out on its own.
        push.send(PAYLOAD).await?;
    }
    // The burst is over: push out whatever is still buffered.
    push.flush().await?;
    // `close` waits for the queued bytes to be written.
    push.close().await?;
    Ok(started)
}

/// The receiving side, on its own thread and runtime. Returns the payload
/// bytes received and when the last message arrived.
fn receive(endpoint_tx: &mpsc::Sender<String>) -> Result<(usize, Instant)> {
    LocalRuntime::new()?.block_on(async {
        let mut pull = PullSocket::new();
        // Port 0: the OS picks a free port and `bind` reports it.
        let endpoint = pull.bind("tcp://127.0.0.1:0").await?;
        endpoint_tx.send(endpoint)?;

        let mut bytes = 0usize;
        for _ in 0..MESSAGES {
            // The `MsgRef` points into the socket's receive slot and gives
            // the slot back when it is dropped at the end of the iteration.
            let msg = pull.recv().await?;
            bytes += msg.byte_len();
        }
        let finished = Instant::now();
        pull.close().await?;
        Ok((bytes, finished))
    })
}
