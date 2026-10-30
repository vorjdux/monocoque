//! Example showing explicit protocol imports with feature flags
//!
//! Run with: `cargo run --example protocol_namespaces --features zmq`
//!
//! This demonstrates how feature-gated protocols keep dependencies minimal.

use monocoque::zmq::Result;

// ============================================================================
// Style 1: Explicit protocol namespace (recommended)
// ============================================================================

#[allow(clippy::future_not_send)]
async fn explicit_style() -> Result<()> {
    use monocoque::zmq::{DealerSocket, RouterSocket};

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    let mut dealer = DealerSocket::new();
    dealer.connect(&endpoint).await?;

    // Clear what protocol each socket uses
    tracing::info!("Created ZMQ DEALER and ROUTER sockets on {endpoint}");
    Ok(())
}

// ============================================================================
// Style 2: Protocol prelude (for convenience)
// ============================================================================

#[allow(clippy::future_not_send)]
async fn prelude_style() -> Result<()> {
    use monocoque::zmq::prelude::*;

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    let mut dealer = DealerSocket::new();
    dealer.connect(&endpoint).await?;

    tracing::info!("Created ZMQ sockets (imported via prelude)");
    Ok(())
}

// ============================================================================
// Future: Multi-protocol mixing (when other protocols are added)
// ============================================================================

#[allow(clippy::future_not_send)]
async fn future_multi_protocol() -> Result<()> {
    // Explicit namespaces prevent conflicts
    use monocoque::zmq::DealerSocket as ZmqDealer;
    // use monocoque::mqtt::Client as MqttClient;  // Future: features = ["mqtt"]
    // use monocoque::amqp::Connection as AmqpConn; // Future: features = ["amqp"]

    // connect returns right away and dials in the background, so this works
    // even with nothing listening yet.
    let mut zmq_socket = ZmqDealer::new();
    zmq_socket.connect("tcp://127.0.0.1:5555").await?;
    // let mqtt_client = MqttClient::connect("mqtt://broker:1883").await?;
    // let amqp_conn = AmqpConn::connect("amqp://localhost:5672").await?;

    tracing::info!("Multiple protocols coexist cleanly!");
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("Import patterns:");
    tracing::info!("  - Explicit: use monocoque::zmq::DealerSocket;");
    tracing::info!("  - Prelude:  use monocoque::zmq::prelude::*;");

    monocoque::rt::LocalRuntime::new()?.block_on(async {
        explicit_style().await?;
        prelude_style().await?;
        future_multi_protocol().await
    })
}
