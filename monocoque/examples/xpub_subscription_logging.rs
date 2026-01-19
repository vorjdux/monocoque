//! XPUB/XSUB Subscription Logging Example
//!
//! This example demonstrates the XPUB socket's ability to receive subscription
//! events from subscribers, which is useful for:
//! - Monitoring what topics subscribers are interested in
//! - Building last value caches (LVC)
//! - Implementing smart message brokers
//! - Subscription auditing and analytics
//!
//! Run this example:
//! ```bash
//! cargo run --example xpub_subscription_logging
//! ```
//!
//! Then in another terminal, run a subscriber:
//! ```bash
//! # Using zmq.rs or libzmq
//! python3 -c "
//! import zmq
//! ctx = zmq.Context()
//! sub = ctx.socket(zmq.SUB)
//! sub.connect('tcp://127.0.0.1:5555')
//! sub.subscribe(b'events.')
//! sub.subscribe(b'alerts.')
//! while True:
//!     msg = sub.recv_multipart()
//!     print(f'Received: {msg}')
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

    info!("Starting XPUB subscription logging example");

    // Create XPUB socket with verbose mode enabled
    let mut xpub = XPubSocket::bind("127.0.0.1:5555").await?;
    xpub.set_verbose(true); // Report all subscription messages

    let addr = xpub.local_addr()?;
    info!("XPUB socket listening on {}", addr);
    info!("Socket type: {:?}", xpub.socket_type());
    info!("");
    info!("Connect subscribers to tcp://127.0.0.1:{}", addr.port());
    info!("Waiting for subscription events...");
    info!("");

    // Track subscription statistics
    let mut total_subscribes = 0u64;
    let mut total_unsubscribes = 0u64;

    loop {
        // Accept new subscribers (non-blocking)
        if let Err(e) = xpub.accept().await {
            if e.kind() != io::ErrorKind::WouldBlock {
                eprintln!("Error accepting connection: {}", e);
            }
        }

        // Check for subscription events
        match xpub.recv_subscription().await? {
            Some(event) => {
                use monocoque_core::subscription::SubscriptionEvent;

                match event {
                    SubscriptionEvent::Subscribe(topic) => {
                        total_subscribes += 1;
                        info!(
                            "📥 SUBSCRIBE: {:?} (total subscribes: {})",
                            String::from_utf8_lossy(&topic),
                            total_subscribes
                        );
                    }
                    SubscriptionEvent::Unsubscribe(topic) => {
                        total_unsubscribes += 1;
                        info!(
                            "📤 UNSUBSCRIBE: {:?} (total unsubscribes: {})",
                            String::from_utf8_lossy(&topic),
                            total_unsubscribes
                        );
                    }
                }
            }
            None => {
                // No subscription events, publish some test data
                if xpub.subscriber_count() > 0 {
                    // Send test messages to all subscribers
                    let topics = vec!["events.login", "events.logout", "alerts.error"];

                    for topic in topics {
                        let msg = vec![
                            Bytes::from(topic),
                            Bytes::from(format!("Test message for {}", topic)),
                        ];

                        if let Err(e) = xpub.send(msg).await {
                            eprintln!("Error sending message: {}", e);
                        }
                    }
                }

                // Small delay to avoid busy-waiting
                compio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}
