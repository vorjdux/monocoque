//! Simple PUB server for interop testing.
//!
//! Binds a PUB socket and publishes a steady stream of `Hello <n>` messages so a
//! late-joining subscriber (the PUB/SUB slow-joiner problem) still receives a
//! run of messages. Runs until the process is terminated.
//!
//! Usage: `pub_server --port <PORT>`

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::PubSocket;
use std::env;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let port = if args.len() > 2 && args[1] == "--port" {
        args[2].parse::<u16>()?
    } else {
        5556
    };

    let mut publisher = PubSocket::bind(format!("127.0.0.1:{port}")).await?;

    // A PUB does not auto-accept subscribers on send(); accept the one the
    // interop harness connects, then publish. Oversending covers the slow-joiner
    // window (the subscription may not be registered when the first sends fire).
    publisher.accept_subscriber().await?;

    let mut n = 0u64;
    loop {
        publisher
            .send(vec![Bytes::from(format!("Hello {n}"))])
            .await?;
        n += 1;
        // A std sleep (not monocoque::rt::sleep) paces the loop without touching
        // the runtime timer state; delivery runs on the PUB worker threads.
        std::thread::sleep(Duration::from_millis(50));
    }
}
