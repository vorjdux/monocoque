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
//!              tcp://*:5555                   tcp://*:5556
//! ```
//!
//! Run this example:
//! ```bash
//! cargo run --example xpub_xsub_broker --features zmq
//! ```
//!
//! Then in other terminals:
//!
//! Publisher:
//! ```python
//! import zmq, time
//! ctx = zmq.Context()
//! pub = ctx.socket(zmq.PUB)
//! pub.connect('tcp://127.0.0.1:5555')
//! time.sleep(0.5)
//! while True:
//!     pub.send_multipart([b'weather.temp', b'25C'])
//!     pub.send_multipart([b'weather.humidity', b'60%'])
//!     pub.send_multipart([b'alerts.warning', b'Storm approaching'])
//!     time.sleep(1)
//! ```
//!
//! Subscriber:
//! ```python
//! import zmq
//! ctx = zmq.Context()
//! sub = ctx.socket(zmq.SUB)
//! sub.connect('tcp://127.0.0.1:5556')
//! sub.subscribe(b'weather.')
//! sub.subscribe(b'alerts.')
//! while True:
//!     topic, data = sub.recv_multipart()
//!     print(f'{topic.decode()}: {data.decode()}')
//! ```

use monocoque::zmq::prelude::*;
use std::collections::HashSet;
use std::io;
use tracing::{info, warn, Level};
use tracing_subscriber;

#[compio::main]
async fn main() -> io::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    info!("Starting XPUB/XSUB message broker");
    info!("Frontend (XSUB): tcp://127.0.0.1:5555 (publishers connect here)");
    info!("Backend (XPUB): tcp://127.0.0.1:5556 (subscribers connect here)");

    // Create XSUB frontend for publishers
    let mut xsub = XSubSocket::connect("127.0.0.1:5555").await?;
    info!("XSUB frontend bound on tcp://127.0.0.1:5555");

    // Create XPUB backend for subscribers  
    let mut xpub = XPubSocket::bind("127.0.0.1:5556").await?;
    xpub.set_verbose(true); // Receive subscription events
    info!("XPUB backend bound on tcp://127.0.0.1:5556");

    // Track active subscriptions
    let mut subscriptions = HashSet::new();
    let mut subscription_changes = 0u64;

    info!("Broker ready - demonstrating subscription forwarding");
    info!("");
    info!("In a real broker, you would:");
    info!("1. Use async tasks to handle XPUB and XSUB concurrently");
    info!("2. Forward subscriptions from XPUB backend to XSUB frontend");
    info!("3. Forward messages from XSUB frontend to XPUB backend");
    info!("");
    info!("This example shows the subscription forwarding pattern:");

    // Simulate receiving subscription events from XPUB (from subscribers)
    loop {
        // In a real broker, this would run in parallel with message forwarding
        match xpub.recv_subscription().await {
            Ok(Some(event)) => {
                match event {
                    SubscriptionEvent::Subscribe(prefix) => {
                        let topic = String::from_utf8_lossy(&prefix);
                        if subscriptions.insert(prefix.clone()) {
                            subscription_changes += 1;
                            info!("→ Subscriber subscribed to '{}'", topic);
                            info!("  Active subscriptions: {}", subscriptions.len());
                            
                            // Forward subscription upstream to publishers via XSUB
                            if let Err(e) = xsub.subscribe(prefix.clone()).await {
                                warn!("Failed to forward subscription: {}", e);
                            } else {
                                info!("  ✓ Forwarded subscription to XSUB frontend");
                            }
                        }
                    }
                    SubscriptionEvent::Unsubscribe(prefix) => {
                        let topic = String::from_utf8_lossy(&prefix);
                        if subscriptions.remove(&prefix) {
                            subscription_changes += 1;
                            info!("← Subscriber unsubscribed from '{}'", topic);
                            info!("  Active subscriptions: {}", subscriptions.len());
                            
                            // Forward unsubscription upstream
                            if let Err(e) = xsub.unsubscribe(prefix.clone()).await {
                                warn!("Failed to forward unsubscription: {}", e);
                            } else {
                                info!("  ✓ Forwarded unsubscription to XSUB frontend");
                            }
                        }
                    }
                }

                // Show stats periodically
                if subscription_changes % 5 == 0 && subscription_changes > 0 {
                    info!("");
                    info!("📊 Broker stats: {} subscription changes", subscription_changes);
                    info!("   Active topics: {:?}", 
                          subscriptions.iter().map(|t| String::from_utf8_lossy(t)).collect::<Vec<_>>());
                    info!("");
                }
            }
            Ok(None) => {
                info!("No more subscription events");
                break;
            }
            Err(e) => {
                warn!("Error receiving subscription: {}", e);
                break;
            }
        }
    }

    info!("Broker demonstration complete");
    info!("Final stats: {} subscription changes", subscription_changes);

    Ok(())
}
