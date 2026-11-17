//! Interoperability: Monocoque REP <-> libzmq REQ
//!
//! A Monocoque REP runs on a background thread and a libzmq REQ makes two
//! requests to it. Needs libzmq installed (the `zmq` crate links it).
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_rep_libzmq --features zmq
//! ```

use monocoque::rt::LocalRuntime;
use monocoque::zmq::RepSocket;
use std::sync::mpsc;
use std::thread;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ROUNDS: usize = 2;

fn main() -> Result<()> {
    println!("=== Monocoque REP <-> libzmq REQ ===\n");

    let (ready_tx, ready_rx) = mpsc::channel();
    let server = thread::spawn(move || -> monocoque::zmq::Result<()> {
        LocalRuntime::new()?.block_on(monocoque_rep(ready_tx))
    });

    // Wait until the server is bound and has reported where. If it failed to
    // bind, the sender is dropped and the join below reports why.
    if let Ok(endpoint) = ready_rx.recv() {
        libzmq_req(&endpoint)?;
    }

    server.join().map_err(|_| "the server thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

async fn monocoque_rep(ready: mpsc::Sender<String>) -> monocoque::zmq::Result<()> {
    let mut socket = RepSocket::new();
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = socket.bind("tcp://127.0.0.1:0").await?;
    println!("[Monocoque REP] Listening on {endpoint}");
    // The main thread waits on this; it only fails if that thread is gone.
    let _ = ready.send(endpoint);

    for round in 1..=ROUNDS {
        {
            // The request is only printed, so it stays borrowed from the
            // socket. The borrow has to end before the reply is sent.
            let request = socket.recv().await?;
            println!("[Monocoque REP] Received request:");
            for (i, frame) in request.frames().enumerate() {
                println!("  Frame {i}: {:?}", String::from_utf8_lossy(frame));
            }
        }
        socket
            .send(format!("Reply {round} from Monocoque REP"))
            .await?;
    }

    // `close` waits for the last reply to be written before the runtime exits.
    socket.close().await
}

fn libzmq_req(endpoint: &str) -> zmq::Result<()> {
    let ctx = zmq::Context::new();
    let req = ctx.socket(zmq::REQ)?;
    req.connect(endpoint)?;
    println!("[libzmq REQ] Connecting to {endpoint}");

    for round in 1..=ROUNDS {
        req.send(&format!("Request {round} from libzmq REQ"), 0)?;
        let reply = req.recv_bytes(0)?;
        println!(
            "[libzmq REQ] Received reply: {:?}",
            String::from_utf8_lossy(&reply)
        );
    }
    Ok(())
}
