//! Example: ROUTER-DEALER load balancer proxy
//!
//! This demonstrates a load balancer that distributes client requests
//! across multiple workers using a message proxy.
//!
//! # Architecture
//!
//! ```text
//! Clients (REQ) -> ROUTER (frontend) -> DEALER (backend) -> Workers (REP)
//!                      |                    |
//!                  Port 5555            Port 5556
//! ```
//!
//! The proxy forwards messages both ways:
//! - Client requests: ROUTER -> DEALER -> Workers
//! - Worker replies: DEALER -> ROUTER -> Clients
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
use monocoque::zmq::proxy::{NoCapture, proxy};
use monocoque::zmq::{DealerSocket, Result, RouterSocket};

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("Starting ROUTER-DEALER load balancer");

    // Frontend: ROUTER accepts client connections (REQ sockets)
    let mut frontend = RouterSocket::new();
    frontend.bind("tcp://127.0.0.1:5555").await?;
    println!("Frontend ROUTER bound to tcp://127.0.0.1:5555 (clients connect with REQ)");

    // Backend: DEALER accepts worker connections (REP sockets)
    let mut backend = DealerSocket::new();
    backend.bind("tcp://127.0.0.1:5556").await?;
    println!("Backend DEALER bound to tcp://127.0.0.1:5556 (workers connect with REP)");

    println!("Proxy running, press Ctrl+C to stop");
    proxy(&mut frontend, &mut backend, None::<&mut NoCapture>).await
}
