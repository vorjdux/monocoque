//! XPUB/XSUB message broker.
//!
//! Publishers and subscribers both connect to the broker, so neither side
//! has to know the other's address:
//!
//! ```text
//! Publishers -> XSUB (frontend) -> proxy() -> XPUB (backend) -> Subscribers
//! ```
//!
//! `proxy` forwards in both directions: published messages go downstream to
//! the subscribers, and their subscriptions go upstream to the publishers.
//! That is why the pair is XSUB/XPUB and not SUB/PUB: with the subscriptions
//! forwarded, a publisher drops a message no subscriber wants before it is
//! ever sent to the broker.
//!
//! To also see the subscriptions as they pass, read them with
//! `recv_subscription` instead: see `xpub_subscription_logging`.
//!
//! The example runs a publisher, the broker and a subscriber in one process:
//! ```bash
//! cargo run --example xpub_xsub_broker --features zmq
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::prelude::*;
use monocoque::zmq::proxy::{NoCapture, proxy};
use std::time::Duration;

/// How many rounds of updates the publisher delivers before it stops.
const ROUNDS: u32 = 3;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Both sides of the broker are bound before anything connects.
    let mut xsub = XSubSocket::new();
    let frontend = xsub.bind("tcp://127.0.0.1:0").await?;
    println!("Frontend (XSUB) on {frontend}: publishers connect here");
    let mut xpub = XPubSocket::new();
    let backend = xpub.bind("tcp://127.0.0.1:0").await?;
    println!("Backend (XPUB) on {backend}: subscribers connect here");

    rt::spawn_detached(async move {
        // `proxy` forwards until one of its sockets fails.
        if let Err(e) = proxy(&mut xsub, &mut xpub, None::<&mut NoCapture>).await {
            eprintln!("Broker stopped: {e}");
        }
    });

    let publishing = rt::spawn(publisher(frontend));

    // A subscriber that only wants weather updates: two per round.
    let mut subscriber = SubSocket::new();
    subscriber.connect(&backend).await?;
    subscriber.subscribe("weather.").await?;
    for _ in 0..ROUNDS * 2 {
        let update = subscriber.recv().await?;
        println!(
            "[Subscriber] {}: {}",
            String::from_utf8_lossy(update.get(0).unwrap_or_default()),
            String::from_utf8_lossy(update.get(1).unwrap_or_default())
        );
    }
    subscriber.close().await?;

    rt::join(publishing).await
}

/// Publish rounds of updates until `ROUNDS` of them have gone out, then stop.
///
/// The publisher knows nothing about the subscribers, only the broker.
async fn publisher(frontend: String) -> Result<()> {
    let mut socket = PubSocket::new();
    socket.connect(&frontend).await?;

    let mut delivered = 0;
    let mut round = 0u32;
    while delivered < ROUNDS {
        round += 1;
        let updates = [
            ("weather.temp", format!("{}C", 20 + round % 10)),
            ("weather.humidity", format!("{}%", 50 + round % 30)),
            ("alerts.warning", "Storm approaching".to_string()),
        ];
        // `send` returns how many peers the message went to. Until the
        // subscription has travelled up through the broker that is none.
        let mut reached = [0usize; 3];
        for (count, (topic, data)) in reached.iter_mut().zip(updates) {
            *count = socket
                .send(Msg::from([Bytes::from(topic), Bytes::from(data)]))
                .await?;
        }
        if reached[0] > 0 {
            delivered += 1;
            println!(
                "[Publisher] Round {delivered}: weather.temp sent to {} peer(s), \
                 weather.humidity to {}, alerts.warning to {} (nobody subscribed)",
                reached[0], reached[1], reached[2]
            );
        }
        rt::sleep(Duration::from_millis(50)).await;
    }
    socket.close().await
}
