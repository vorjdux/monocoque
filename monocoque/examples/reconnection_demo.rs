//! Example demonstrating automatic reconnection.
//!
//! A connected socket redials on its own when the connection is lost, backing
//! off between attempts from `reconnect_ivl` up to `reconnect_ivl_max`. The
//! client below never reconnects by hand: it watches its monitor for
//! Connected/Disconnected events while the server goes away and comes back on
//! the same port, and its next request simply waits for the new connection.
//!
//! Run this example:
//! ```bash
//! cargo run --example reconnection_demo --features zmq
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{RepSocket, ReqSocket, Result, SocketEvent, SocketOptions};
use std::time::Duration;
use tracing::info;

/// Bind a REP server on `endpoint` that answers `count` requests and then
/// closes. Returns the bound endpoint and the server task.
#[allow(clippy::future_not_send)]
async fn start_server(
    endpoint: &str,
    count: usize,
) -> Result<(String, rt::JoinHandle<Result<()>>)> {
    let mut rep = RepSocket::new();
    let bound = rep.bind(endpoint).await?;
    info!("[Server] Listening on {}", bound);
    let task = rt::spawn(async move {
        for _ in 0..count {
            let request = rep.recv_owned().await?;
            let text = String::from_utf8_lossy(request.first().unwrap_or_default()).into_owned();
            rep.send(format!("Reply to {text}")).await?;
        }
        rep.close().await?;
        info!("[Server] Shut down");
        Ok(())
    });
    Ok((bound, task))
}

#[allow(clippy::future_not_send)]
async fn request(req: &mut ReqSocket, n: usize) -> Result<()> {
    let message = format!("Request #{n}");
    info!("[Client] Sending: {}", message);
    req.send(message).await?;
    let reply = req.recv().await?;
    info!(
        "[Client] Received: {}",
        String::from_utf8_lossy(reply.first().unwrap_or_default())
    );
    Ok(())
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    LocalRuntime::new()?.block_on(async {
        let (endpoint, first_server) = start_server("tcp://127.0.0.1:0", 2).await?;

        // Configure reconnection with exponential backoff
        let options = SocketOptions::default()
            .with_reconnect_ivl(Duration::from_millis(100)) // Start with 100ms
            .with_reconnect_ivl_max(Duration::from_secs(2)) // Cap at 2 seconds
            .with_recv_timeout(Duration::from_secs(5));
        let mut req = ReqSocket::with_options(options);

        // Print connection events as they happen
        let monitor = req.monitor();
        rt::spawn_detached(async move {
            while let Ok(event) = monitor.recv_async().await {
                if matches!(
                    event,
                    SocketEvent::Connected(_) | SocketEvent::Disconnected(_)
                ) {
                    info!("[Monitor] {event}");
                }
            }
        });

        req.connect(&endpoint).await?;
        request(&mut req, 1).await?;
        request(&mut req, 2).await?;

        // The first server answers two requests and goes away.
        rt::join(first_server).await?;

        // Bring a new server up on the same port after a pause. Meanwhile the
        // client keeps redialing in the background.
        rt::sleep(Duration::from_millis(500)).await;
        let (_, second_server) = start_server(&endpoint, 2).await?;

        // These requests wait for the reconnection, no retry loop needed.
        request(&mut req, 3).await?;
        request(&mut req, 4).await?;

        rt::join(second_server).await?;
        info!("Client finished successfully");
        Ok(())
    })
}
