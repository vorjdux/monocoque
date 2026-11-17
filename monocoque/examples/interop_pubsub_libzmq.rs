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
use std::thread;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ENDPOINT: &str = "tcp://127.0.0.1:5564";
const EVENTS: usize = 3;

fn main() -> Result<()> {
    println!("=== Monocoque PUB <-> libzmq SUB ===\n");

    // libzmq keeps dialing until the publisher is bound, so the subscriber
    // can start first.
    let subscriber = thread::spawn(libzmq_sub);

    LocalRuntime::new()?.block_on(monocoque_pub())?;

    subscriber
        .join()
        .map_err(|_| "the libzmq thread panicked")??;
    println!("\nInterop run completed.");
    Ok(())
}

fn libzmq_sub() -> zmq::Result<()> {
    let ctx = zmq::Context::new();
    let sub = ctx.socket(zmq::SUB)?;
    sub.connect(ENDPOINT)?;
    sub.set_subscribe(b"topic.event")?;
    println!("[libzmq SUB] Subscribed to 'topic.event'");

    for _ in 0..EVENTS {
        let msg = sub.recv_bytes(0)?;
        println!("[libzmq SUB] Received: {:?}", String::from_utf8_lossy(&msg));
    }
    Ok(())
}

async fn monocoque_pub() -> monocoque::zmq::Result<()> {
    let mut publisher = PubSocket::new();
    publisher.bind(ENDPOINT).await?;
    println!("[Monocoque PUB] Listening on {ENDPOINT}");

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
