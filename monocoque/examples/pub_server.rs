//! Simple PUB server for interop testing.
//!
//! Binds a PUB socket and publishes a steady stream of `Hello <n>` messages so a
//! late-joining subscriber (the PUB/SUB slow-joiner problem) still receives a
//! run of messages. Runs until the process is terminated.
//!
//! Usage: `pub_server --port <PORT>`

use monocoque::rt::{self, LocalRuntime};
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

    let mut publisher = PubSocket::new();
    publisher.bind(&format!("tcp://127.0.0.1:{port}")).await?;

    // Subscribers are accepted in the background. Messages published before a
    // subscription arrives are dropped, so keep publishing to cover the
    // slow-joiner window.
    let mut n = 0u64;
    loop {
        publisher.send(format!("Hello {n}")).await?;
        n += 1;
        rt::sleep(Duration::from_millis(50)).await;
    }
}
