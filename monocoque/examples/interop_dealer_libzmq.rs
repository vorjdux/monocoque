//! Interoperability: Monocoque DEALER <-> libzmq ROUTER
//!
//! A libzmq ROUTER runs on a background thread and a Monocoque DEALER talks
//! to it. Needs libzmq installed (the `zmq` crate links it).
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_dealer_libzmq --features zmq
//! ```

use monocoque::rt::LocalRuntime;
use monocoque::zmq::DealerSocket;
use std::thread;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ENDPOINT: &str = "tcp://127.0.0.1:5560";

fn main() -> Result<()> {
    println!("=== Monocoque DEALER <-> libzmq ROUTER ===\n");

    let server = thread::spawn(libzmq_router);

    // Connecting does not wait for the server: the socket dials in the
    // background and the send waits for the peer.
    LocalRuntime::new()?.block_on(monocoque_dealer())?;

    server.join().map_err(|_| "the libzmq thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

fn libzmq_router() -> zmq::Result<()> {
    let ctx = zmq::Context::new();
    let router = ctx.socket(zmq::ROUTER)?;
    router.bind(ENDPOINT)?;
    println!("[libzmq ROUTER] Listening on {ENDPOINT}");

    // A ROUTER receives the sender's identity ahead of the body.
    let identity = router.recv_bytes(0)?;
    let body = router.recv_bytes(0)?;
    println!(
        "[libzmq ROUTER] Received {:?} from a peer with a {}-byte identity",
        String::from_utf8_lossy(&body),
        identity.len()
    );

    router.send(&identity, zmq::SNDMORE)?;
    router.send("Pong from libzmq", 0)?;
    println!("[libzmq ROUTER] Sent reply");
    // Dropping the context waits for the reply to be written.
    Ok(())
}

async fn monocoque_dealer() -> monocoque::zmq::Result<()> {
    let mut dealer = DealerSocket::new();
    dealer.connect(ENDPOINT).await?;
    println!("[Monocoque DEALER] Connecting to {ENDPOINT}");

    // The ROUTER adds the identity frame on its side.
    dealer.send("Ping from Monocoque").await?;
    println!("[Monocoque DEALER] Sent request");

    {
        // The reply is only printed, so it stays borrowed from the socket.
        let response = dealer.recv().await?;
        println!("[Monocoque DEALER] Received response:");
        for (i, frame) in response.frames().enumerate() {
            println!("  Frame {i}: {:?}", String::from_utf8_lossy(frame));
        }
    }
    dealer.close().await
}
