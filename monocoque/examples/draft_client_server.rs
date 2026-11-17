//! CLIENT/SERVER: request-reply addressed by routing id (draft).
//!
//! SERVER is the draft replacement for ROUTER and CLIENT for DEALER. Instead
//! of an identity frame in front of the message, a SERVER receive returns
//! the sender's routing id, a number the SERVER assigned to that connection,
//! next to the message. Sending to that id reaches the same client. Messages
//! are single-part: sending more than one frame is an error.
//!
//! Like ROUTER/DEALER and unlike REQ/REP there is no lockstep: a SERVER may
//! reply to any client at any time, or not at all.
//!
//! The draft socket types follow libzmq's draft API, which may still change,
//! so they sit behind the `draft` feature. Both ends have to be draft-aware.
//!
//! Run with: `cargo run --example draft_client_server --features zmq,draft`

use monocoque::rt::LocalRuntime;
use monocoque::zmq::Result;
use monocoque::zmq::draft::{ClientSocket, ServerSocket};

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut server = ServerSocket::new();
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = server.bind("tcp://127.0.0.1:0").await?;
    println!("[SERVER] listening on {endpoint}");

    let mut alice = ClientSocket::new();
    alice.connect(&endpoint).await?;
    let mut bob = ClientSocket::new();
    bob.connect(&endpoint).await?;

    // A send waits for the connection, so there is nothing to wait for here.
    alice.send("hello from alice").await?;
    bob.send("hello from bob").await?;

    for _ in 0..2 {
        // The request is only read, so it stays borrowed from the socket.
        // The routing id is a plain number and outlives the borrow.
        let (routing_id, reply) = {
            let (routing_id, request) = server.recv().await?;
            let text = String::from_utf8_lossy(request.first().unwrap_or_default());
            println!("[SERVER] routing id {routing_id:#010x} sent \"{text}\"");
            (routing_id, format!("server got \"{text}\""))
        };
        server.send(routing_id, reply).await?;
    }

    // Each client gets the reply to its own request.
    for (name, client) in [("alice", &mut alice), ("bob", &mut bob)] {
        let reply = client.recv().await?;
        println!(
            "[{name}] {}",
            String::from_utf8_lossy(reply.first().unwrap_or_default())
        );
    }

    alice.close().await?;
    bob.close().await?;
    server.close().await
}
