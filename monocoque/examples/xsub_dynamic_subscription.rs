//! XSUB Dynamic Subscription Example
//!
//! This example demonstrates the XSUB socket's ability to send subscription
//! messages upstream to publishers, enabling:
//! - Dynamic topic subscription at runtime
//! - Subscription forwarding in message brokers
//! - Cascading publish-subscribe networks
//!
//! An in-process PUB publishes a steady stream on three topics while the XSUB
//! changes what it subscribes to over time. Run this example:
//! ```bash
//! cargo run --example xsub_dynamic_subscription --features zmq
//! ```

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::prelude::*;
use std::time::{Duration, Instant};
use tracing::{Level, info};

/// Run each step for this long before moving to the next one.
const STEP: Duration = Duration::from_millis(400);

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("Starting XSUB dynamic subscription example");

    // Publisher
    let mut publisher = PubSocket::new();
    let endpoint = publisher.bind("tcp://127.0.0.1:0").await?;
    rt::spawn_detached(async move {
        loop {
            for (topic, data) in [
                ("events.login", "User logged in"),
                ("events.logout", "User logged out"),
                ("alerts.error", "System error"),
            ] {
                if publisher
                    .send(Msg::from([Bytes::from(topic), Bytes::from(data)]))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            rt::sleep(Duration::from_millis(100)).await;
        }
    });

    // A short receive timeout lets the loop below move on to the next step
    // even while nothing matches.
    let mut xsub = XSubSocket::with_options(
        SocketOptions::default().with_recv_timeout(Duration::from_millis(50)),
    );
    xsub.connect(&endpoint).await?;

    info!("Connected to publisher at {}", endpoint);
    info!("Socket type: {:?}", xsub.socket_type());

    // Dynamically change subscriptions over time
    let subscription_plan: [(&str, bool, &str); 4] = [
        ("events.", true, "Subscribing to all events"),
        ("alerts.", true, "Adding alerts subscription"),
        ("events.", false, "Unsubscribing from events"),
        ("", true, "Subscribing to ALL topics"),
    ];

    for (topic, subscribe, description) in subscription_plan {
        info!("");
        info!("{}", description);
        if subscribe {
            // An empty prefix subscribes to everything
            xsub.subscribe(topic).await?;
        } else {
            xsub.unsubscribe(topic).await?;
        }

        let start = Instant::now();
        while start.elapsed() < STEP {
            match xsub.recv().await {
                Ok(msg) => {
                    let topic = String::from_utf8_lossy(msg.get(0).unwrap_or_default());
                    let payload = String::from_utf8_lossy(msg.get(1).unwrap_or_default());
                    info!("  Received: [{}] {}", topic, payload);
                }
                Err(MonocoqueError::TimedOut) => {}
                Err(e) => return Err(e),
            }
        }
    }

    info!("");
    info!("Demonstration complete!");
    Ok(())
}
