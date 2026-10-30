//! Socket Introspection Example
//!
//! Demonstrates the socket introspection API:
//! - `socket_type()` - Get the socket type
//! - `last_endpoint()` - Get the most recently bound endpoint
//! - `peer_count()` and `peer_property()` - Inspect connected peers
//! - TCP keepalive options
//!
//! Run this example:
//! ```bash
//! cargo run --example socket_introspection --features zmq
//! ```

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, Result, RouterSocket, SocketOptions};

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    println!("=== Socket Introspection Demo ===\n");

    // Stand up a ROUTER peer so the DEALER has something to connect to.
    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;

    // 1. Socket Type Introspection
    println!("## Socket Type Introspection");
    let mut dealer = DealerSocket::new();
    dealer.connect(&endpoint).await?;
    println!("[Dealer] Socket type: {:?}", dealer.socket_type());
    println!("[Dealer] Type name: {}", dealer.socket_type().as_str());

    // 2. Endpoint Introspection
    println!("\n## Endpoint Introspection");
    match router.last_endpoint() {
        Some(endpoint) => println!("[Router] Bound to: {endpoint}"),
        None => println!("[Router] No endpoint information available"),
    }

    // 3. Peer Introspection. A round trip makes sure the link is up.
    println!("\n## Peer Introspection");
    dealer
        .send(["part one", "part two"].map(Bytes::from))
        .await?;
    let msg = router.recv().await?;
    println!(
        "[Router] Received {} frames (identity + 2 parts)",
        msg.len()
    );
    println!("[Router] Peers: {}", router.peer_count());
    if let Some(kind) = router.peer_property("Socket-Type") {
        println!(
            "[Router] Peer Socket-Type: {}",
            String::from_utf8_lossy(&kind)
        );
    }

    // 4. TCP Keepalive Options
    println!("\n## TCP Keepalive Configuration");
    let opts = SocketOptions::default()
        .with_tcp_keepalive(1) // Enable keepalive
        .with_tcp_keepalive_cnt(5) // 5 probes before timeout
        .with_tcp_keepalive_idle(60) // Start probing after 60s idle
        .with_tcp_keepalive_intvl(10); // 10s between probes

    println!("TCP Keepalive Options:");
    println!("  - Enabled: {}", opts.tcp_keepalive);
    println!("  - Probe count: {}", opts.tcp_keepalive_cnt);
    println!("  - Idle time: {}s", opts.tcp_keepalive_idle);
    println!("  - Interval: {}s", opts.tcp_keepalive_intvl);

    // 5. Create socket with options
    println!("\n## Socket with Custom Options");
    let router_opts = SocketOptions::default()
        .with_routing_id(Bytes::from_static(b"my-router"))
        .with_router_mandatory(true)
        .with_conflate(false);

    println!("Router Options:");
    println!("  - Routing ID: {:?}", router_opts.routing_id);
    println!("  - Router Mandatory: {}", router_opts.router_mandatory);
    println!("  - Conflate: {}", router_opts.conflate);

    // 6. REQ Socket Modes
    println!("\n## REQ Socket Modes");
    let req_opts = SocketOptions::default()
        .with_req_correlate(true) // Match replies to requests
        .with_req_relaxed(false); // Strict send-recv alternation

    println!("REQ Options:");
    println!("  - Correlate: {}", req_opts.req_correlate);
    println!("  - Relaxed: {}", req_opts.req_relaxed);

    // 7. Runtime options modification
    println!("\n## Runtime Options Modification");
    let current_hwm = dealer.options().send_hwm;
    println!("[Dealer] Current send HWM: {current_hwm}");

    dealer.update_options(|o| o.with_send_hwm(500));
    println!("[Dealer] New send HWM: {}", dealer.options().send_hwm);

    println!("\n=== Demo Complete ===");
    println!("\nKey Points:");
    println!("  - socket_type() returns the ZeroMQ socket type");
    println!("  - last_endpoint() shows where the socket bound");
    println!("  - peer_count() and peer_property() describe connected peers");
    println!("  - TCP keepalive options enable connection monitoring");
    println!("  - REQ modes control request-reply behavior");
    println!("  - Options can be modified at runtime via update_options()");

    Ok(())
}
