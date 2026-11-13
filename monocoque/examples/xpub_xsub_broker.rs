//! XPUB/XSUB Message Broker Example
//!
//! This example demonstrates a complete message broker pattern using XPUB and XSUB sockets.
//! The broker forwards subscriptions upstream and messages downstream, enabling:
//! - Centralized message routing
//! - Dynamic topic-based filtering
//! - Subscription aggregation
//! - Scalable pub/sub architecture
//!
//! Architecture:
//! ```text
//! Publishers → XSUB (frontend) → Broker → XPUB (backend) → Subscribers
//! ```
//!
//! The example runs a publisher, the broker and a subscriber in one process:
//! ```bash
//! cargo run --example xpub_xsub_broker --features zmq
//! ```
//!
//! External peers can join too, for example a pyzmq subscriber:
//! ```python
//! import zmq
//! ctx = zmq.Context()
//! sub = ctx.socket(zmq.SUB)
//! sub.connect('tcp://127.0.0.1:<backend port printed by the example>')
//! sub.subscribe(b'weather.')
//! while True:
//!     topic, data = sub.recv_multipart()
//!     print(f'{topic.decode()}: {data.decode()}')
//! ```

use bytes::Bytes;
use futures::future::{Either, select};
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::SubscriptionEvent;
use monocoque::zmq::prelude::*;
use std::collections::HashSet;
use std::pin::pin;
use std::time::Duration;
use tracing::{Level, info};

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    info!("Starting XPUB/XSUB message broker");

    // Create XSUB frontend for publishers
    let mut xsub = XSubSocket::new();
    let frontend = xsub.bind("tcp://127.0.0.1:0").await?;
    info!("Frontend (XSUB): {} (publishers connect here)", frontend);

    // Create XPUB backend for subscribers, reporting every subscription message
    let mut xpub = XPubSocket::with_options(SocketOptions::default().with_xpub_verbose(true));
    let backend = xpub.bind("tcp://127.0.0.1:0").await?;
    info!("Backend (XPUB): {} (subscribers connect here)", backend);

    rt::spawn_detached(async move {
        if let Err(e) = broker(xsub, xpub).await {
            info!("Broker stopped: {e}");
        }
    });

    // A publisher that knows nothing about the subscribers
    rt::spawn_detached(async move {
        let mut publisher = PubSocket::new();
        publisher.connect(&frontend).await.unwrap();
        let mut n = 0u64;
        loop {
            n += 1;
            for (topic, data) in [
                ("weather.temp", format!("{}C", 20 + n % 10)),
                ("weather.humidity", format!("{}%", 50 + n % 30)),
                ("alerts.warning", "Storm approaching".to_string()),
            ] {
                publisher
                    .send(Msg::from([Bytes::from(topic), Bytes::from(data)]))
                    .await
                    .unwrap();
            }
            rt::sleep(Duration::from_millis(50)).await;
        }
    });

    // A subscriber that only wants weather updates
    let mut sub = SubSocket::new();
    sub.connect(&backend).await?;
    sub.subscribe(b"weather.").await?;
    for _ in 0..6 {
        let msg = sub.recv().await?;
        info!(
            "[Subscriber] {}: {}",
            String::from_utf8_lossy(msg.get(0).unwrap_or_default()),
            String::from_utf8_lossy(msg.get(1).unwrap_or_default())
        );
    }

    info!("Broker demonstration complete");
    Ok(())
}

/// Forward subscriptions upstream and messages downstream until an error.
#[allow(clippy::future_not_send)]
async fn broker(mut xsub: XSubSocket, mut xpub: XPubSocket) -> Result<()> {
    // Track active subscriptions
    let mut subscriptions = HashSet::new();

    loop {
        // Receives are cancel safe: a message is only taken when it is ready.
        let next = {
            let down = pin!(xpub.recv_owned());
            let up = pin!(xsub.recv_owned());
            match select(down, up).await {
                Either::Left((msg, _)) => Either::Left(msg?),
                Either::Right((msg, _)) => Either::Right(msg?),
            }
        };

        match next {
            // A subscription change from a subscriber
            Either::Left(msg) => {
                match msg.first().and_then(SubscriptionEvent::from_message) {
                    Some(SubscriptionEvent::Subscribe(prefix))
                        if subscriptions.insert(prefix.clone()) =>
                    {
                        info!(
                            "Subscriber subscribed to '{}' (active: {})",
                            String::from_utf8_lossy(&prefix),
                            subscriptions.len()
                        );
                    }
                    Some(SubscriptionEvent::Unsubscribe(prefix))
                        if subscriptions.remove(&prefix) =>
                    {
                        info!(
                            "Subscriber unsubscribed from '{}' (active: {})",
                            String::from_utf8_lossy(&prefix),
                            subscriptions.len()
                        );
                    }
                    _ => {}
                }
                // Forward the raw subscription message upstream to publishers
                xsub.send(msg).await?;
            }
            // A published message: fan it out to matching subscribers
            Either::Right(msg) => {
                xpub.send(msg).await?;
            }
        }
    }
}
