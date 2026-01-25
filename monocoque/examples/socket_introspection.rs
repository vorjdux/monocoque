//! Socket Introspection Example
//!
//! Demonstrates the new socket introspection API:
//! - socket_type() - Get the socket type
//! - last_endpoint() - Get connected/bound endpoint
//! - has_more() - Check for multipart message continuation
//! - TCP keepalive options
//!
//! Run this example:
//! ```bash
//! cargo run --example socket_introspection
//! ```

use bytes::Bytes;
use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
use std::time::Duration;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Socket Introspection Demo ===\n");

    // 1. Socket Type Introspection
    println!("## Socket Type Introspection");
    let mut dealer = DealerSocket::connect("127.0.0.1:5556").await?;
    println!("[Dealer] Socket type: {:?}", dealer.socket_type());
    println!("[Dealer] Type name: {}", dealer.socket_type().as_str());
    
    // Wait a bit
    compio::time::sleep(Duration::from_millis(100)).await;

    // 2. Endpoint Introspection
    println!("\n## Endpoint Introspection");
    if let Some(endpoint) = dealer.last_endpoint() {
        println!("[Dealer] Connected to: {}", endpoint);
    } else {
        println!("[Dealer] No endpoint information available");
    }

    // 3. TCP Keepalive Options
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

    // 4. Create socket with options
    println!("\n## Socket with Custom Options");
    let mut router_opts = SocketOptions::default()
        .with_routing_id(Bytes::from_static(b"my-router"))
        .with_router_mandatory(true)
        .with_conflate(false);

    println!("Router Options:");
    println!("  - Routing ID: {:?}", router_opts.routing_id);
    println!("  - Router Mandatory: {}", router_opts.router_mandatory);
    println!("  - Conflate: {}", router_opts.conflate);

    // 5. REQ Socket Modes
    println!("\n## REQ Socket Modes");
    let req_opts = SocketOptions::default()
        .with_req_correlate(true) // Match replies to requests
        .with_req_relaxed(false); // Strict send-recv alternation

    println!("REQ Options:");
    println!("  - Correlate: {}", req_opts.req_correlate);
    println!("  - Relaxed: {}", req_opts.req_relaxed);

    // 6. Runtime options modification
    println!("\n## Runtime Options Modification");
    let current_hwm = dealer.options().send_hwm;
    println!("[Dealer] Current send HWM: {}", current_hwm);
    
    dealer.options_mut().send_hwm = 500;
    println!("[Dealer] New send HWM: {}", dealer.options().send_hwm);

    // 7. Multipart message check
    println!("\n## Multipart Message Check");
    println!("[Dealer] Has more frames: {}", dealer.has_more());
    println!("  (This will be true if the last received message was multipart)");

    println!("\n=== Demo Complete ===");
    println!("\nKey Points:");
    println!("  • socket_type() returns the ZeroMQ socket type");
    println!("  • last_endpoint() shows where the socket connected/bound");
    println!("  • TCP keepalive options enable connection monitoring");
    println!("  • REQ modes control request-reply behavior");
    println!("  • Options can be modified at runtime via options_mut()");
    
    Ok(())
}
