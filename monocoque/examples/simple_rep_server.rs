//! Simple REP Server for Interop Testing
//!
//! Binds a REP socket and echoes every request back with an "Echo: " prefix on
//! the first frame. Runs until the process is terminated.
//!
//! Usage: `simple_rep_server --port <PORT>`

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, RepSocket};
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable debug logging.
    // SAFETY: set as the very first thing in `main`, before any other thread is
    // spawned, so there is no concurrent access to the process environment.
    unsafe {
        std::env::set_var("RUST_LOG", "debug");
    }
    tracing_subscriber::fmt::init();
    // Parse port from args (default 5555)
    let args: Vec<String> = env::args().collect();
    let port = if args.len() > 2 && args[1] == "--port" {
        args[2].parse::<u16>()?
    } else {
        5555
    };

    let mut socket = RepSocket::new();
    let endpoint = socket.bind(&format!("tcp://127.0.0.1:{port}")).await?;
    println!("REP server listening on {endpoint}");

    // Echo loop
    loop {
        let reply = {
            let msg = socket.recv().await?;
            println!("Received {} frames", msg.len());

            // Echo back with "Echo: " prefix on first frame
            let mut reply = Msg::new();
            match msg.first() {
                None => reply.push(Bytes::from("Echo: <empty>")),
                Some(first) => {
                    reply.push(Bytes::from(format!(
                        "Echo: {}",
                        String::from_utf8_lossy(first)
                    )));
                    // Copy remaining frames as-is
                    for i in 1..msg.len() {
                        if let Some(frame) = msg.frame_bytes(i) {
                            reply.push(frame);
                        }
                    }
                }
            }
            reply
        };
        socket.send(reply).await?;
    }
}
