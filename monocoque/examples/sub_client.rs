//! Simple SUB client for interop testing.
//!
//! Connects to a publisher, subscribes (to `--topic` if given, else everything),
//! and prints each received message to stdout. The output is flushed after every
//! message so that when the interop harness sends SIGTERM, the messages received
//! so far are already in the pipe (a plain `println!` to a pipe is block-buffered
//! and would be lost on an abrupt exit).
//!
//! Usage: `sub_client --port <PORT> [--topic <PREFIX>]`

use monocoque::rt::LocalRuntime;
use monocoque::zmq::SubSocket;
use std::env;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let mut port: u16 = 5556;
    let mut topic = String::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(port);
                i += 2;
            }
            "--topic" => {
                topic = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            _ => i += 1,
        }
    }

    // The harness may start this subscriber before the publisher binds; connect
    // keeps retrying in the background until the endpoint is up.
    let mut sub = SubSocket::new();
    sub.connect(&format!("tcp://127.0.0.1:{port}")).await?;
    sub.subscribe(topic.as_bytes()).await?;

    let mut out = std::io::stdout();
    loop {
        let msg = sub.recv().await?;
        for frame in msg.frames() {
            writeln!(out, "Received: {}", String::from_utf8_lossy(frame))?;
        }
        out.flush()?;
    }
}
