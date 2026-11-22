//! One program, any runtime backend, no code changes.
//!
//! This runs a PUSH/PULL round trip through `monocoque::rt::LocalRuntime`, the
//! runtime-agnostic entry point. The exact same source compiles and runs on
//! all three backends; you pick the backend with a Cargo feature:
//!
//! ```bash
//! # io_uring via compio (default)
//! cargo run --example runtime_backends --features zmq
//!
//! # tokio (current-thread, portable)
//! cargo run --example runtime_backends --no-default-features --features runtime-tokio,zmq
//!
//! # smol (async-executor + async-io)
//! cargo run --example runtime_backends --no-default-features --features runtime-smol,zmq
//! ```
//!
//! `LocalRuntime` wraps a single-threaded runtime (a compio runtime, a
//! current-thread tokio runtime inside a `LocalSet`, or a smol `LocalExecutor`),
//! so `rt::spawn` and the socket calls behave the same on every backend.

use monocoque::zmq::{PullSocket, PushSocket, Result};

fn main() -> Result<()> {
    let rt = monocoque::rt::LocalRuntime::new()?;

    rt.block_on(async {
        // Bind and connect both return right away; the connection is made in
        // the background and the first send waits for it.
        let mut push = PushSocket::new();
        let endpoint = push.bind("tcp://127.0.0.1:0").await?;
        let mut pull = PullSocket::new();
        pull.connect(&endpoint).await?;

        let backend = if cfg!(feature = "runtime-tokio") {
            "tokio"
        } else if cfg!(feature = "runtime-smol") {
            "smol"
        } else {
            "compio (io_uring)"
        };
        println!("running on the {backend} backend");

        push.send("hello from monocoque").await?;

        let msg = pull.recv().await?;
        let text = String::from_utf8_lossy(msg.first().unwrap_or_default());
        println!("received: {text}");

        Ok(())
    })
}
