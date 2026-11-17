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
use std::thread;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ENDPOINT: &str = "tcp://127.0.0.1:5562";
const ROUNDS: usize = 2;

fn main() -> Result<()> {
    println!("=== Monocoque REQ <-> libzmq REP ===\n");

    let server = thread::spawn(libzmq_rep);

    // The REQ socket dials in the background until the server is up, so
    // there is no need to wait for the bind.
    LocalRuntime::new()?.block_on(monocoque_req())?;

    server.join().map_err(|_| "the libzmq thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

fn libzmq_rep() -> zmq::Result<()> {
    let ctx = zmq::Context::new();
    let rep = ctx.socket(zmq::REP)?;
    rep.bind(ENDPOINT)?;
    println!("[libzmq REP] Listening on {ENDPOINT}");

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

async fn monocoque_req() -> monocoque::zmq::Result<()> {
    let mut socket = ReqSocket::new();
    socket.connect(ENDPOINT).await?;
    println!("[Monocoque REQ] Connecting to {ENDPOINT}");

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
