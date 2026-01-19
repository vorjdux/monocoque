//! XSUB Dynamic Subscription Example
//!
//! This example demonstrates the XSUB socket's ability to send subscription
//! messages upstream to publishers, enabling:
//! - Dynamic topic subscription at runtime
//! - Subscription forwarding in message brokers
//! - Cascading publish-subscribe networks
//!
//! Run this example:
//! ```bash
//! cargo run --example xsub_dynamic_subscription
//! ```
//!
//! First start a publisher:
//! ```bash
//! # Using zmq.rs or libzmq
//! python3 -c "
//! import zmq
//! import time
//! ctx = zmq.Context()
//! pub = ctx.socket(zmq.PUB)
//! pub.bind('tcp://127.0.0.1:5556')
//! time.sleep(1)  # Let subscribers connect
//! while True:
//!     pub.send_multipart([b'events.login', b'User logged in'])
//!     pub.send_multipart([b'events.logout', b'User logged out'])
//!     pub.send_multipart([b'alerts.error', b'System error'])
//!     time.sleep(1)
//! "
//! ```

use bytes::Bytes;
use monocoque::zmq::prelude::*;
use std::io;
use tracing::{info, Level};
use tracing_subscriber;

#[compio::main]
async fn main() -> io::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    info!("Starting XSUB dynamic subscription example");

    // Connect to publisher
    let mut xsub = XSubSocket::connect("127.0.0.1:5556").await?;

    info!("Connected to publisher at tcp://127.0.0.1:5556");
    info!("Socket type: {:?}", xsub.socket_type());
    info!("");

    // Dynamically subscribe to different topics over time
    let subscription_plan = vec![
        (2, "events.", "Subscribing to all events"),
        (4, "alerts.", "Adding alerts subscription"),
        (6, "events.", "Unsubscribing from events"),
        (8, "", "Subscribing to ALL topics"),
    ];

    let start = std::time::Instant::now();
    let mut next_action_idx = 0;

    loop {
        let elapsed = start.elapsed().as_secs();

        // Check if it's time for next subscription action
        if next_action_idx < subscription_plan.len() {
            let (trigger_sec, topic, description) = &subscription_plan[next_action_idx];
            if elapsed >= *trigger_sec {
                info!("⏰ [{}s] {}", elapsed, description);

                if topic.is_empty() {
                    // Empty prefix = subscribe to all
                    xsub.subscribe(Bytes::new()).await?;
                } else if next_action_idx == 2 {
                    // Unsubscribe example
                    let topic_bytes = Bytes::from_static(b"events.");
                    xsub.unsubscribe(&topic_bytes).await?;
                } else {
                    // Normal subscribe
                    xsub.subscribe(Bytes::from(*topic)).await?;
                }

                info!("   Active subscriptions: {}", xsub.subscription_count());
                next_action_idx += 1;
            }
        }

        // Try to receive messages
        match xsub.recv().await? {
            Some(msg) => {
                if msg.is_empty() {
                    continue;
                }

                let topic = String::from_utf8_lossy(&msg[0]);
                let payload = if msg.len() > 1 {
                    String::from_utf8_lossy(&msg[1])
                } else {
                    "".into()
                };

                info!("📨 Received: [{}] {}", topic, payload);
            }
            None => {
                // No message available, small delay
                compio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        // Exit after demonstration
        if elapsed > 10 {
            info!("");
            info!("✅ Demonstration complete!");
            info!("   Final subscription count: {}", xsub.subscription_count());
            break;
        }
    }

    Ok(())
}
