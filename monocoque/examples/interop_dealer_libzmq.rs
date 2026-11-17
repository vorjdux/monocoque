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
use std::sync::mpsc;
use std::thread;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    println!("=== Monocoque DEALER <-> libzmq ROUTER ===\n");

    let (bound_tx, bound_rx) = mpsc::channel();
    let server = thread::spawn(move || libzmq_router(&bound_tx));

    // The server reports the endpoint it bound. If it failed before that,
    // its end of the channel is gone and the join below reports why.
    if let Ok(endpoint) = bound_rx.recv() {
        LocalRuntime::new()?.block_on(monocoque_dealer(&endpoint))?;
    }

    server.join().map_err(|_| "the libzmq thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

fn libzmq_router(bound: &mpsc::Sender<String>) -> zmq::Result<()> {
    let ctx = zmq::Context::new();
    let router = ctx.socket(zmq::ROUTER)?;
    // A `*` port lets the OS pick a free one; the last endpoint says which.
    router.bind("tcp://127.0.0.1:*")?;
    let endpoint = router
        .get_last_endpoint()?
        .map_err(|_| zmq::Error::EINVAL)?;
    println!("[libzmq ROUTER] Listening on {endpoint}");
    // The main thread waits on this; it only fails if that thread is gone.
    let _ = bound.send(endpoint);

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

async fn monocoque_dealer(endpoint: &str) -> monocoque::zmq::Result<()> {
    let mut dealer = DealerSocket::new();
    dealer.connect(endpoint).await?;
    println!("[Monocoque DEALER] Connecting to {endpoint}");

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
