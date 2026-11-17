//! Interoperability: Monocoque PUB <-> libzmq SUB
//!
//! A Monocoque PUB publishes to a libzmq SUB running on a background thread.
//! Needs libzmq installed (the `zmq` crate links it).
//!
//! Run this example:
//! ```bash
//! cargo run --example interop_pubsub_libzmq --features zmq
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::PubSocket;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const EVENTS: usize = 3;

fn main() -> Result<()> {
    println!("=== Monocoque PUB <-> libzmq SUB ===\n");

    // The publisher binds a port the OS picks and tells the subscriber's
    // thread where to connect.
    let (bound_tx, bound_rx) = mpsc::channel();
    let subscriber = thread::spawn(move || libzmq_sub(&bound_rx));

    LocalRuntime::new()?.block_on(monocoque_pub(bound_tx))?;

    subscriber
        .join()
        .map_err(|_| "the libzmq thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

fn libzmq_sub(bound: &mpsc::Receiver<String>) -> zmq::Result<()> {
    // If the publisher failed to bind, its end of the channel is gone and
    // there is nothing to subscribe to; `main` reports the bind error.
    let Ok(endpoint) = bound.recv() else {
        return Ok(());
    };
    let ctx = zmq::Context::new();
    let sub = ctx.socket(zmq::SUB)?;
    sub.connect(&endpoint)?;
    sub.set_subscribe(b"topic.event")?;
    println!("[libzmq SUB] Subscribed to 'topic.event'");

    for _ in 0..EVENTS {
        let msg = sub.recv_bytes(0)?;
        println!("[libzmq SUB] Received: {:?}", String::from_utf8_lossy(&msg));
    }
    Ok(())
}

async fn monocoque_pub(bound: mpsc::Sender<String>) -> monocoque::zmq::Result<()> {
    let mut publisher = PubSocket::new();
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = publisher.bind("tcp://127.0.0.1:0").await?;
    println!("[Monocoque PUB] Listening on {endpoint}");
    // The subscriber's thread waits on this; it only fails if that thread
    // is gone.
    let _ = bound.send(endpoint);

    // A PUB drops messages nobody is subscribed to, and `send` returns how
    // many subscribers a message went to. Publish a probe on a topic the
    // subscriber matches until its subscription has arrived.
    while publisher.send("topic.event.probe").await? == 0 {
        rt::sleep(Duration::from_millis(10)).await;
    }
    println!("[Monocoque PUB] Subscriber is listening");

    // The probe was the first event the subscriber received.
    for i in 1..EVENTS {
        let message = format!("topic.event.{i}");
        println!("[Monocoque PUB] Publishing: {message:?}");
        publisher.send(message).await?;
    }

    // `close` waits for the queued events to be written.
    publisher.close().await
}
