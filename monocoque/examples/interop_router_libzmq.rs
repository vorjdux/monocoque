//! Interoperability: Monocoque ROUTER <-> libzmq DEALER
//!
//! A Monocoque ROUTER runs on a background thread and a libzmq DEALER talks
//! to it. Needs libzmq installed (the `zmq` crate links it).
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_router_libzmq --features zmq
//! ```

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, RouterSocket};
use std::sync::mpsc;
use std::thread;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ENDPOINT: &str = "tcp://127.0.0.1:5561";

fn main() -> Result<()> {
    println!("=== Monocoque ROUTER <-> libzmq DEALER ===\n");

    let (ready_tx, ready_rx) = mpsc::channel();
    let server = thread::spawn(move || -> monocoque::zmq::Result<()> {
        LocalRuntime::new()?.block_on(monocoque_router(ready_tx))
    });

    // Wait until the server is bound. If it failed to bind, the sender is
    // dropped and the join below reports why.
    if ready_rx.recv().is_ok() {
        libzmq_dealer()?;
    }

    server.join().map_err(|_| "the server thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

async fn monocoque_router(ready: mpsc::Sender<()>) -> monocoque::zmq::Result<()> {
    let mut router = RouterSocket::new();
    router.bind(ENDPOINT).await?;
    println!("[Monocoque ROUTER] Listening on {ENDPOINT}");
    // The main thread waits on this; it only fails if that thread is gone.
    let _ = ready.send(());

    // A ROUTER receives the sender's identity as the first frame. The message
    // is only read, so it stays borrowed and just the identity is kept.
    let identity = {
        let msg = router.recv().await?;
        println!("[Monocoque ROUTER] Received message:");
        println!(
            "  Identity (frame 0): {:?}",
            String::from_utf8_lossy(msg.first().unwrap_or_default())
        );
        for (i, frame) in msg.frames().enumerate().skip(1) {
            println!("  Frame {i}: {:?}", String::from_utf8_lossy(frame));
        }
        msg.frame_bytes(0).unwrap_or_default()
    };

    // The reply's first frame is the identity to route to.
    let reply = Msg::from([identity, Bytes::from_static(b"Reply from Monocoque ROUTER")]);
    router.send(reply).await?;
    println!("[Monocoque ROUTER] Sent reply");

    // `close` waits for the reply to be written before the runtime exits.
    router.close().await
}

fn libzmq_dealer() -> zmq::Result<()> {
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER)?;
    dealer.set_identity(b"CLIENT_123")?;
    dealer.connect(ENDPOINT)?;
    println!("[libzmq DEALER] Connecting to {ENDPOINT} with identity 'CLIENT_123'");

    dealer.send("Request from libzmq", 0)?;
    println!("[libzmq DEALER] Sent request");

    let reply = dealer.recv_bytes(0)?;
    println!(
        "[libzmq DEALER] Received reply: {:?}",
        String::from_utf8_lossy(&reply)
    );
    Ok(())
}
