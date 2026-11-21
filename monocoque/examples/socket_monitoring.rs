//! Socket monitoring: lifecycle events from a socket.
//!
//! `monitor()` returns a channel of `SocketEvent`s: binds, accepted and
//! established connections, disconnects and failures. Use it to log what a
//! socket's connections are doing, or to wait for one to be up instead of
//! sleeping and hoping.
//!
//! Take the monitor before the socket binds or connects, so it sees every
//! event from the start. The channel is bounded and the socket never waits
//! on it: events are dropped if nobody reads them.
//!
//! Run with: `cargo run --example socket_monitoring --features zmq`

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, RouterSocket, SocketEvent, SocketMonitor};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut router = RouterSocket::new();
    let router_events = router.monitor();
    let mut dealer = DealerSocket::new();
    let dealer_events = dealer.monitor();

    println!("1. Binding the ROUTER");
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    wait_for("ROUTER", &router_events, |event| {
        matches!(event, SocketEvent::Listening(_))
    })
    .await?;

    println!("2. Connecting the DEALER");
    // `connect` returns once dialing has started. The events say when the
    // connection is really there, on each side.
    dealer.connect(&endpoint).await?;
    wait_for("DEALER", &dealer_events, |event| {
        matches!(event, SocketEvent::Connected(_))
    })
    .await?;
    wait_for("ROUTER", &router_events, |event| {
        matches!(event, SocketEvent::Accepted(_))
    })
    .await?;

    println!("3. Exchanging a message (no events: the connection is unchanged)");
    dealer.send("ping").await?;
    {
        let msg = router.recv().await?;
        println!("  ROUTER received {} frames, identity included", msg.len());
    }

    println!("4. Closing the ROUTER");
    router.close().await?;
    wait_for("DEALER", &dealer_events, |event| {
        matches!(event, SocketEvent::Disconnected(_))
    })
    .await?;

    println!("5. Dropping the DEALER");
    drop(dealer);
    // The channel closes with its socket. Whatever was still queued is read
    // first, then the receive fails.
    while let Ok(event) = dealer_events.recv_async().await {
        describe("DEALER", &event);
    }
    println!("  [DEALER] monitor closed");
    Ok(())
}

/// Print events from `monitor` up to and including the first one that
/// `wanted` accepts.
async fn wait_for(
    name: &str,
    monitor: &SocketMonitor,
    wanted: impl Fn(&SocketEvent) -> bool,
) -> Result<()> {
    loop {
        let event = monitor.recv_async().await?;
        describe(name, &event);
        if wanted(&event) {
            return Ok(());
        }
    }
}

fn describe(name: &str, event: &SocketEvent) {
    match event {
        SocketEvent::Bound(endpoint) => println!("  [{name}] bound to {endpoint}"),
        SocketEvent::Listening(endpoint) => println!("  [{name}] listening on {endpoint}"),
        SocketEvent::Accepted(endpoint) => println!("  [{name}] accepted a peer on {endpoint}"),
        SocketEvent::Connected(endpoint) => println!("  [{name}] connected to {endpoint}"),
        SocketEvent::Disconnected(endpoint) => {
            println!("  [{name}] disconnected from {endpoint}");
        }
        SocketEvent::BindFailed { endpoint, reason } => {
            println!("  [{name}] bind to {endpoint} failed: {reason}");
        }
        SocketEvent::ConnectFailed { endpoint, reason } => {
            println!("  [{name}] connect to {endpoint} failed: {reason}");
        }
        SocketEvent::PeerStalled(endpoint) => {
            println!("  [{name}] the peer on {endpoint} stopped reading");
        }
        // The enum can grow. Every event also implements `Display`.
        other => println!("  [{name}] {other}"),
    }
}
