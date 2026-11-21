//! Taking connections down: `disconnect`, `unbind`, and giving up on a dial.
//!
//! `connect` and `bind` have opposites, and a socket's monitor shows what each
//! one does:
//!
//! - `disconnect(endpoint)` drops the connection and stops redialing it. Both
//!   ends report `Disconnected`.
//! - `unbind(endpoint)` stops accepting on it. Peers already accepted stay,
//!   and there is no event for it.
//! - A socket normally redials a dead endpoint forever.
//!   `with_max_reconnect_attempts` makes it stop after that many failures in
//!   a row, each reported as `ConnectFailed`. `with_connect_timeout` bounds
//!   how long one dial may take before it counts as failed.
//!
//! Use these when the set of endpoints changes while the program runs, or
//! when a client should notice that a server is gone instead of waiting.
//!
//! Run with: `cargo run --example connect_disconnect --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, RouterSocket, SocketEvent, SocketMonitor, SocketOptions};
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ATTEMPTS: u32 = 3;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut router = RouterSocket::new();
    let router_events = router.monitor();
    let mut dealer = DealerSocket::new();
    let dealer_events = dealer.monitor();

    println!("1. bind and connect");
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    dealer.connect(&endpoint).await?;
    wait_for("DEALER", &dealer_events, |e| {
        matches!(e, SocketEvent::Connected(_))
    })
    .await?;
    wait_for("ROUTER", &router_events, |e| {
        matches!(e, SocketEvent::Accepted(_))
    })
    .await?;

    println!("2. unbind: no new peers, the accepted one stays");
    router.unbind(&endpoint)?;
    dealer.send("still here").await?;
    {
        let msg = router.recv().await?;
        println!(
            "  [ROUTER] received {:?} from the peer it already had",
            String::from_utf8_lossy(msg.last().unwrap_or_default())
        );
    }

    println!("3. disconnect: the connection goes and is not redialed");
    dealer.disconnect(&endpoint)?;
    wait_for("DEALER", &dealer_events, |e| {
        matches!(e, SocketEvent::Disconnected(_))
    })
    .await?;
    wait_for("ROUTER", &router_events, |e| {
        matches!(e, SocketEvent::Disconnected(_))
    })
    .await?;
    println!(
        "  peers now: DEALER {}, ROUTER {}",
        dealer.peer_count(),
        router.peer_count()
    );

    println!("4. connect where nothing listens, giving up after {ATTEMPTS} attempts");
    let mut late = DealerSocket::with_options(
        SocketOptions::default()
            .with_max_reconnect_attempts(Some(ATTEMPTS))
            // A refused dial fails at once. A dial that gets no answer at all
            // would count as failed after this long.
            .with_connect_timeout(Duration::from_millis(500))
            .with_reconnect_ivl(Duration::from_millis(50)),
    );
    let late_events = late.monitor();
    // The endpoint was unbound in step 2, so every dial is refused.
    late.connect(&endpoint).await?;
    for _ in 0..ATTEMPTS {
        wait_for("LATE", &late_events, |e| {
            matches!(e, SocketEvent::ConnectFailed { .. })
        })
        .await?;
    }
    // Several reconnect intervals pass without another attempt.
    match rt::timeout(Duration::from_millis(500), late_events.recv_async()).await {
        Ok(Ok(event)) => println!("  [LATE] unexpected: {event}"),
        _ => println!("  [LATE] no further attempts"),
    }

    late.close().await?;
    dealer.close().await?;
    router.close().await?;
    Ok(())
}

/// Print events from `monitor` up to and including the first one that
/// `wanted` accepts. Every event implements `Display`.
async fn wait_for(
    name: &str,
    monitor: &SocketMonitor,
    wanted: impl Fn(&SocketEvent) -> bool,
) -> Result<()> {
    loop {
        let event = rt::timeout(Duration::from_secs(5), monitor.recv_async())
            .await
            .map_err(|_| format!("{name}: no event within 5 s"))??;
        println!("  [{name}] {event}");
        if wanted(&event) {
            return Ok(());
        }
    }
}
