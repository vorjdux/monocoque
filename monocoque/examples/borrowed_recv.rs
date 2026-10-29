//! Borrowed receive: `recv()` lends the message instead of handing it over.
//!
//! `recv()` returns a `MsgRef` that points at the socket's receive slot. The
//! frames are read in place, nothing is copied, and dropping the `MsgRef`
//! releases the slot for the next message, so a receive loop allocates nothing
//! per message. To keep a message past the next receive, call
//! `MsgRef::take()` or use `recv_owned()`, which moves it out as a `Msg`.
//!
//! Run with: `cargo run --example borrowed_recv --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{Msg, PullSocket, PushSocket};
use std::time::Instant;

const MESSAGES: usize = 200_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

#[allow(clippy::cast_precision_loss)]
async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pull = PullSocket::new();
    let endpoint = pull.bind("tcp://127.0.0.1:0").await?;

    // Sender: push MESSAGES plus two keepers, then close, which waits for
    // everything queued to be written.
    rt::spawn_detached(async move {
        let mut push = PushSocket::new();
        push.connect(&endpoint).await.unwrap();
        for _ in 0..MESSAGES {
            push.send("task").await.unwrap();
        }
        push.send("keep me").await.unwrap();
        push.send("keep me too").await.unwrap();
        push.close().await.unwrap();
    });

    // Receiver: each `MsgRef` is read in place and dropped at the end of the
    // iteration, which hands the slot back for the next message.
    let mut bytes = 0usize;
    let start = Instant::now();
    for _ in 0..MESSAGES {
        let msg = pull.recv().await?;
        bytes += msg.first().map_or(0, <[u8]>::len);
    }
    let elapsed = start.elapsed();

    println!(
        "received {MESSAGES} messages ({bytes} bytes) by reference in {:.1} ms ({:.2} M msg/s)",
        elapsed.as_secs_f64() * 1000.0,
        MESSAGES as f64 / elapsed.as_secs_f64() / 1e6,
    );

    // Keeping a message: `take()` moves it out of the slot...
    let kept: Msg = pull.recv().await?.take();
    // ...and `recv_owned()` does the same in one call.
    let kept_too: Msg = pull.recv_owned().await?;

    assert_eq!(kept, [b"keep me"]);
    assert_eq!(kept_too, [b"keep me too"]);
    println!(
        "kept {:?} and {:?} as owned messages",
        String::from_utf8_lossy(kept.first().unwrap_or_default()),
        String::from_utf8_lossy(kept_too.first().unwrap_or_default()),
    );
    Ok(())
}
