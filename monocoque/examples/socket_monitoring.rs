//! Demonstrates socket lifecycle event monitoring.
//!
//! This example shows how to create and use a socket monitor to track
//! connection events like connects, disconnects, binds, etc.

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, Endpoint, Result, RouterSocket, SocketEvent, SocketMonitor};
use std::time::Duration;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

/// Print every event from `monitor` in a background task.
fn watch(name: &'static str, monitor: SocketMonitor) {
    rt::spawn_detached(async move {
        while let Ok(event) = monitor.recv_async().await {
            println!("  [{name}] {event}");
        }
    });
}

async fn async_main() -> Result<()> {
    println!("Socket Monitor Example");
    println!("======================\n");

    // A monitor is taken from the socket before it binds or connects, so it
    // sees every event from the start.
    let mut router = RouterSocket::new();
    watch("ROUTER", router.monitor());
    let mut dealer = DealerSocket::new();
    watch("DEALER", dealer.monitor());

    println!("1. Binding the ROUTER");
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    rt::sleep(Duration::from_millis(50)).await;

    println!("2. Connecting the DEALER");
    dealer.connect(&endpoint).await?;
    dealer.send("ping").await?;
    let _ = router.recv().await?;
    rt::sleep(Duration::from_millis(50)).await;

    println!("3. Closing the ROUTER");
    router.close().await?;
    rt::sleep(Duration::from_millis(200)).await;

    println!("4. Dropping the DEALER");
    drop(dealer);
    rt::sleep(Duration::from_millis(50)).await;

    println!("\nOther event types:");
    let tcp_ep = Endpoint::parse("tcp://127.0.0.1:5555").unwrap();
    let bind_ep = Endpoint::parse("tcp://0.0.0.0:6666").unwrap();
    let peer_ep = Endpoint::parse("tcp://192.168.1.100:12345").unwrap();

    println!("  {}", SocketEvent::Listening(bind_ep.clone()));
    println!("  {}", SocketEvent::Accepted(peer_ep));
    println!(
        "  {}",
        SocketEvent::BindFailed {
            endpoint: bind_ep,
            reason: "Address already in use".to_string()
        }
    );
    println!(
        "  {}",
        SocketEvent::ConnectFailed {
            endpoint: tcp_ep,
            reason: "Connection refused".to_string()
        }
    );

    println!("\nSocket monitoring example completed");
    Ok(())
}
