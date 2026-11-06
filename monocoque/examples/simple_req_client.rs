//! Simple REQ Client for Interop Testing
//!
//! Connects to a REP server, sends one request and prints the reply.
//!
//! Usage: `simple_req_client --port <PORT>`

use monocoque::rt::LocalRuntime;
use monocoque::zmq::ReqSocket;
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse port from args (default 5555)
    let args: Vec<String> = env::args().collect();
    let port = if args.len() > 2 && args[1] == "--port" {
        args[2].parse::<u16>()?
    } else {
        5555
    };

    // Connect to server. The dial runs in the background; the send below waits
    // for the connection.
    let addr = format!("tcp://127.0.0.1:{port}");
    let mut socket = ReqSocket::new();
    socket.connect(&addr).await?;
    println!("Connecting to {addr}");

    // Send request
    socket.send("Hello from Monocoque").await?;
    println!("Sent: Hello from Monocoque");

    // Receive reply
    let reply = socket.recv_owned().await?;
    println!(
        "Received: {}",
        String::from_utf8_lossy(reply.first().unwrap_or_default())
    );

    socket.close().await?;
    Ok(())
}
