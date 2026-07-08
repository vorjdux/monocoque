//! Example: ROUTER-DEALER load balancer proxy
//!
//! This demonstrates a load balancer that distributes client requests
//! across multiple workers using a message proxy.
//!
//! # Architecture
//!
//! ```text
//! Clients (REQ) → ROUTER (frontend) → DEALER (backend) → Workers (REP)
//!                      ↓                    ↓
//!                  Port 5555            Port 5556
//! ```
//!
//! The proxy forwards messages bidirectionally:
//! - Client requests: ROUTER → DEALER → Workers
//! - Worker replies: DEALER → ROUTER → Clients
//!
//! Run this example:
//! ```bash
//! cargo run --example proxy_broker --features zmq
//! ```
//!
//! Then in separate terminals:
//! - REQ clients connect to 5555 and send requests
//! - REP workers connect to 5556 and process requests

use monocoque::rt::LocalRuntime;
use monocoque::zmq::proxy::proxy;
use monocoque::zmq::{DealerSocket, RouterSocket};

fn main() -> std::io::Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> std::io::Result<()> {
    // Enable logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("🚀 Starting ROUTER-DEALER Load Balancer");
    println!("========================================\n");
    println!("Frontend: ROUTER listening on tcp://127.0.0.1:5555");
    println!("  → Clients connect with REQ sockets\n");
    println!("Backend:  DEALER listening on tcp://127.0.0.1:5556");
    println!("  → Workers connect with REP sockets\n");

    // Frontend: ROUTER accepts client connections (REQ sockets)
    let (_, mut frontend) = RouterSocket::bind("127.0.0.1:5555").await?;
    println!("✓ Frontend ROUTER bound to 5555");

    // Backend: DEALER accepts worker connections (REP sockets)
    let (_, mut backend) = DealerSocket::bind("127.0.0.1:5556").await?;
    println!("✓ Backend DEALER bound to 5556");

    println!("\n📡 Proxy running... Press Ctrl+C to stop\n");

    // Run the proxy (forwards requests and replies bidirectionally)
    proxy(
        &mut frontend,
        &mut backend,
        Option::<&mut RouterSocket>::None,
    )
    .await?;

    Ok(())
}
