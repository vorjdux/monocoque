//! Coalesced broadcast: one PUB publishing a burst to several subscribers.
//!
//! A PUB is eager by default: every `send` writes to every matching
//! subscriber before it returns. That gives the lowest latency and costs one
//! write per subscriber per message. For a burst of small messages, turn on
//! write coalescing: each subscriber's messages are batched into a few large
//! writes, and `flush()` pushes out the rest when the burst is over.
//!
//! `send_ref` publishes a message the caller keeps, so a message published
//! again and again is built once and never cloned.
//!
//! For the plain version with a topic per subscriber and eager sends, see
//! `pubsub_multi`.
//!
//! Run with: `cargo run --example multi_subscriber --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::prelude::*;
use std::time::{Duration, Instant};

const SUBSCRIBERS: usize = 3;
const BROADCASTS: usize = 50_000;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let options = SocketOptions::default()
        .with_write_coalescing(true)
        // A PUB never waits: a subscriber whose queue is at the HWM misses
        // the message. This burst is published faster than it can be written,
        // so the queue is sized to hold all of it and nothing is dropped.
        .with_send_hwm(BROADCASTS);
    let mut publisher = PubSocket::with_options(options);
    let endpoint = publisher.bind("tcp://127.0.0.1:0").await?;
    println!("Publisher bound to {endpoint}");

    let subscribers: Vec<_> = (1..=SUBSCRIBERS)
        .map(|id| rt::spawn(subscriber(id, endpoint.clone())))
        .collect();

    // A PUB drops whatever nobody is subscribed to yet, so wait until every
    // subscriber is connected and its subscription has arrived.
    loop {
        let peers = publisher.peer_ids();
        if peers.len() == SUBSCRIBERS
            && peers
                .iter()
                .all(|&peer| publisher.subscriber_prefixes(peer) > 0)
        {
            break;
        }
        rt::sleep(Duration::from_millis(5)).await;
    }

    // Built once, published by reference every time.
    let tick = Msg::from([
        Bytes::from_static(b"ticks.demo"),
        Bytes::from_static(b"a small payload"),
    ]);

    println!("Publishing {BROADCASTS} messages to {SUBSCRIBERS} subscribers...");
    let start = Instant::now();
    for _ in 0..BROADCASTS {
        publisher.send_ref(&tick).await?;
    }
    // The burst is over: push out whatever is still buffered.
    publisher.flush().await?;

    for handle in subscribers {
        rt::join(handle).await?;
    }
    println!(
        "All subscribers received {BROADCASTS} messages in {:.1} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    publisher.close().await
}

async fn subscriber(id: usize, endpoint: String) -> Result<()> {
    let mut socket = SubSocket::new();
    socket.connect(&endpoint).await?;
    socket.subscribe("ticks.").await?;

    let mut bytes = 0usize;
    for _ in 0..BROADCASTS {
        // The message is only read, so it stays borrowed from the socket.
        let msg = socket.recv().await?;
        bytes += msg.byte_len();
    }
    println!("[SUB{id}] Received {BROADCASTS} messages ({bytes} bytes)");
    socket.close().await
}
