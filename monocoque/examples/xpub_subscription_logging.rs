//! XPUB/XSUB Subscription Logging Example
//!
//! This example demonstrates the XPUB socket's ability to receive subscription
//! events from subscribers, which is useful for:
//! - Monitoring what topics subscribers are interested in
//! - Building last value caches (LVC)
//! - Implementing smart message brokers
//! - Subscription auditing and analytics
//!
//! An in-process SUB subscribes, receives, and drops one subscription, and the
//! XPUB logs every change. Run this example:
//! ```bash
//! cargo run --example xpub_subscription_logging --features zmq
//! ```
//!
//! An external subscriber works the same way, for example with pyzmq:
//! ```python
//! import zmq
//! ctx = zmq.Context()
//! sub = ctx.socket(zmq.SUB)
//! sub.connect('tcp://127.0.0.1:<port printed by the example>')
//! sub.subscribe(b'events.')
//! ```

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::SubscriptionEvent;
use monocoque::zmq::prelude::*;
use tracing::{Level, info};

const TOPICS: [&str; 3] = ["events.login", "events.logout", "alerts.error"];

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    info!("Starting XPUB subscription logging example");

    // Create XPUB socket with verbose mode enabled (report every subscription
    // message, not only the first per topic)
    let mut xpub = XPubSocket::with_options(SocketOptions::default().with_xpub_verbose(true));
    let endpoint = xpub.bind("tcp://127.0.0.1:0").await?;
    info!("XPUB socket listening on {}", endpoint);
    info!("Socket type: {:?}", xpub.socket_type());

    let subscriber = rt::spawn(async move { run_subscriber(&endpoint).await });

    // Track subscription statistics
    let mut total_subscribes = 0u64;
    let mut total_unsubscribes = 0u64;
    let mut log = |event: SubscriptionEvent| match event {
        SubscriptionEvent::Subscribe(topic) => {
            total_subscribes += 1;
            info!(
                "SUBSCRIBE: {:?} (total subscribes: {})",
                String::from_utf8_lossy(&topic),
                total_subscribes
            );
        }
        SubscriptionEvent::Unsubscribe(topic) => {
            total_unsubscribes += 1;
            info!(
                "UNSUBSCRIBE: {:?} (total unsubscribes: {})",
                String::from_utf8_lossy(&topic),
                total_unsubscribes
            );
        }
    };

    // The subscriber registers two prefixes.
    log(xpub.recv_subscription().await?);
    log(xpub.recv_subscription().await?);
    publish(&mut xpub).await?;

    // After its first batch it drops "alerts.", so the next batch reaches it
    // only for the "events." topics.
    log(xpub.recv_subscription().await?);
    publish(&mut xpub).await?;

    rt::join(subscriber).await?;
    xpub.close().await?;
    info!("Done");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn publish(xpub: &mut XPubSocket) -> Result<()> {
    for topic in TOPICS {
        let msg = Msg::from([
            Bytes::from(topic),
            Bytes::from(format!("Test message for {topic}")),
        ]);
        let delivered = xpub.send(msg).await?;
        info!("Published {topic} to {delivered} subscriber(s)");
    }
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn run_subscriber(endpoint: &str) -> Result<()> {
    let mut sub = SubSocket::new();
    sub.connect(endpoint).await?;
    sub.subscribe(b"events.").await?;
    sub.subscribe(b"alerts.").await?;

    for _ in 0..TOPICS.len() {
        let msg = sub.recv().await?;
        info!(
            "  [SUB] Received {:?}",
            String::from_utf8_lossy(msg.first().unwrap_or_default())
        );
    }

    sub.unsubscribe(b"alerts.").await?;
    for _ in 0..2 {
        let msg = sub.recv().await?;
        info!(
            "  [SUB] Received {:?}",
            String::from_utf8_lossy(msg.first().unwrap_or_default())
        );
    }
    Ok(())
}
