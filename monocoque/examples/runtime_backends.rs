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

use bytes::Bytes;
use monocoque::zmq::{PullSocket, PushSocket};

fn main() -> std::io::Result<()> {
    let rt = monocoque::rt::LocalRuntime::new()?;

    rt.block_on(async {
        let addr = "127.0.0.1:5599";

        // `PushSocket::bind` binds and then accepts one peer in the same call,
        // so the PULL connect has to run concurrently. `join` drives both on the
        // single-threaded runtime: the bind future reaches its accept point
        // before the connect future is polled, so there is no connect race.
        let (push_res, pull_res) =
            futures::join!(PushSocket::bind(addr), PullSocket::connect(addr));

        let (_listener, mut push) = push_res?;
        let mut pull = pull_res?;

        let backend = if cfg!(feature = "runtime-tokio") {
            "tokio"
        } else if cfg!(feature = "runtime-smol") {
            "smol"
        } else {
            "compio (io_uring)"
        };
        println!("running on the {backend} backend");

        push.send(vec![Bytes::from_static(b"hello from monocoque")])
            .await?;

        if let Some(msg) = pull.recv().await? {
            let text = String::from_utf8_lossy(&msg[0]);
            println!("received: {text}");
        }

        Ok(())
    })
}
