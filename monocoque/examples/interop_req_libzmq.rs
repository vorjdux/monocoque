//! Interoperability: Monocoque REQ <-> libzmq REP
//!
//! A libzmq REP runs on a background thread and a Monocoque REQ makes two
//! requests to it. Needs libzmq installed (the `zmq` crate links it).
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_req_libzmq --features zmq
//! ```

use monocoque::rt::LocalRuntime;
use monocoque::zmq::ReqSocket;
use std::sync::mpsc;
use std::thread;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ROUNDS: usize = 2;

fn main() -> Result<()> {
    println!("=== Monocoque REQ <-> libzmq REP ===\n");

    let (bound_tx, bound_rx) = mpsc::channel();
    let server = thread::spawn(move || libzmq_rep(&bound_tx));

    // The server reports the endpoint it bound. If it failed before that,
    // its end of the channel is gone and the join below reports why.
    if let Ok(endpoint) = bound_rx.recv() {
        LocalRuntime::new()?.block_on(monocoque_req(&endpoint))?;
    }

    server.join().map_err(|_| "the libzmq thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

fn libzmq_rep(bound: &mpsc::Sender<String>) -> zmq::Result<()> {
    let ctx = zmq::Context::new();
    let rep = ctx.socket(zmq::REP)?;
    // A `*` port lets the OS pick a free one; the last endpoint says which.
    rep.bind("tcp://127.0.0.1:*")?;
    let endpoint = rep.get_last_endpoint()?.map_err(|_| zmq::Error::EINVAL)?;
    println!("[libzmq REP] Listening on {endpoint}");
    // The main thread waits on this; it only fails if that thread is gone.
    let _ = bound.send(endpoint);

    for round in 1..=ROUNDS {
        let request = rep.recv_bytes(0)?;
        println!(
            "[libzmq REP] Received request: {:?}",
            String::from_utf8_lossy(&request)
        );
        rep.send(&format!("Reply {round} from libzmq REP"), 0)?;
    }
    // Dropping the context waits for the last reply to be written.
    Ok(())
}

async fn monocoque_req(endpoint: &str) -> monocoque::zmq::Result<()> {
    let mut socket = ReqSocket::new();
    socket.connect(endpoint).await?;
    println!("[Monocoque REQ] Connecting to {endpoint}");

    for round in 1..=ROUNDS {
        socket
            .send(format!("Request {round} from Monocoque REQ"))
            .await?;
        // The reply is only printed, so it stays borrowed from the socket.
        let reply = socket.recv().await?;
        println!("[Monocoque REQ] Received reply:");
        for (i, frame) in reply.frames().enumerate() {
            println!("  Frame {i}: {:?}", String::from_utf8_lossy(frame));
        }
    }
    socket.close().await
}
