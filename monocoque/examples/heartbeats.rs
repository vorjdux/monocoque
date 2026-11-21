//! Heartbeats: noticing a dead peer on an idle connection.
//!
//! A TCP connection that carries no traffic looks the same whether the peer
//! is alive or has vanished behind a dropped link. With `heartbeat_ivl` set,
//! the socket sends a ZMTP PING on a connection that has been silent for
//! that long, and the peer's socket answers with a PONG on its own. If
//! nothing arrives within `heartbeat_timeout`, the connection is closed and,
//! for a connecting socket, redialed.
//!
//! Use it on long-lived connections that can sit idle, most of all across
//! NATs and firewalls that forget quiet connections. Only the side that
//! wants the check needs the options.
//!
//! This example leaves a DEALER/ROUTER pair idle for a few heartbeat
//! intervals and uses the monitor to show that the connection stayed up.
//!
//! Run with: `cargo run --example heartbeats --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, RouterSocket, SocketEvent, SocketOptions};
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(200);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(600);
const IDLE: Duration = Duration::from_secs(2);

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut router = RouterSocket::new();
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;

    let options = SocketOptions::default()
        .with_heartbeat_ivl(HEARTBEAT_INTERVAL)
        // Without this the timeout is one interval. A few intervals of
        // slack keeps one late PONG from dropping a healthy connection.
        .with_heartbeat_timeout(HEARTBEAT_TIMEOUT);
    let mut dealer = DealerSocket::with_options(options);
    // Taken before connecting, so it sees every event from the start.
    let events = dealer.monitor();
    dealer.connect(&endpoint).await?;

    loop {
        let event = events.recv_async().await?;
        println!("[DEALER] {event}");
        if matches!(event, SocketEvent::Connected(_)) {
            break;
        }
    }

    println!(
        "Idle for {IDLE:?}: a PING every {HEARTBEAT_INTERVAL:?}, \
         the connection is dropped after {HEARTBEAT_TIMEOUT:?} of silence"
    );
    rt::sleep(IDLE).await;

    // A missed heartbeat would have closed the connection, and the monitor
    // would hold a `Disconnected` event by now.
    let mut dropped = false;
    while let Ok(event) = events.try_recv() {
        println!("[DEALER] {event}");
        dropped |= matches!(event, SocketEvent::Disconnected(_));
    }
    println!(
        "Connection dropped while idle: {dropped}; peers connected: {}",
        dealer.peer_count()
    );

    // The same connection still carries messages.
    dealer.send("still here").await?;
    {
        let msg = router.recv().await?;
        println!(
            "[ROUTER] got {:?}",
            String::from_utf8_lossy(msg.last().unwrap_or_default())
        );
    }

    dealer.close().await?;
    router.close().await?;
    Ok(())
}
