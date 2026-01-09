//! Example showing explicit protocol imports with feature flags
//!
//! Run with: `cargo run --example protocol_namespaces --features zmq`
//!
//! This demonstrates how feature-gated protocols keep dependencies minimal.

use tracing::info;

// ============================================================================
// Style 1: Explicit protocol namespace (recommended)
// ============================================================================

#[allow(dead_code)]
async fn explicit_style() -> Result<(), Box<dyn std::error::Error>> {
    use monocoque::zmq::{DealerSocket, RouterSocket};

    let dealer = DealerSocket::connect("127.0.0.1:5555").await?;
    let (listener, router) = RouterSocket::bind("127.0.0.1:6666").await?;

    // Clear what protocol each socket uses
    tracing::info!("Created ZMQ DEALER and ROUTER sockets");

    drop((dealer, router, listener));
    Ok(())
}

// ============================================================================
// Style 2: Protocol prelude (for convenience)
// ============================================================================

#[allow(dead_code)]
async fn prelude_style() -> Result<(), Box<dyn std::error::Error>> {
    use monocoque::zmq::prelude::*;

    let dealer = DealerSocket::connect("127.0.0.1:5555").await?;
    let (listener, router) = RouterSocket::bind("127.0.0.1:6666").await?;

    tracing::info!("Created ZMQ sockets (imported via prelude)");

    drop((dealer, router, listener));
    Ok(())
}

// ============================================================================
// Future: Multi-protocol mixing (when other protocols are added)
// ============================================================================

#[allow(dead_code)]
async fn future_multi_protocol() -> Result<(), Box<dyn std::error::Error>> {
    // Explicit namespaces prevent conflicts
    use monocoque::zmq::DealerSocket as ZmqDealer;
    // use monocoque::mqtt::Client as MqttClient;  // Future: features = ["mqtt"]
    // use monocoque::amqp::Connection as AmqpConn; // Future: features = ["amqp"]

    let zmq_socket = ZmqDealer::connect("127.0.0.1:5555").await?;
    // let mqtt_client = MqttClient::connect("mqtt://broker:1883").await?;
    // let amqp_conn = AmqpConn::connect("amqp://localhost:5672").await?;

    tracing::info!("Multiple protocols coexist cleanly!");

    drop(zmq_socket);
    Ok(())
}

fn main() {
    tracing::info!("Example requires 'zmq' feature:");
    tracing::info!("  cargo run --example protocol_namespaces --features zmq");
    tracing::info!("");
    tracing::info!("Import patterns:");
    tracing::info!("  - Explicit: use monocoque::zmq::DealerSocket;");
    tracing::info!("  - Prelude:  use monocoque::zmq::prelude::*;");
}
