//! Paranoid Pirate Pattern - Simple Demo
//!
//! This demonstrates the key concepts of the Paranoid Pirate pattern:
//! - Workers send READY and HEARTBEAT messages
//! - Simple request-reply through broker
//! - Worker crash and recovery
//!
//! Simplified for single-threaded compio runtime.

use bytes::Bytes;
use monocoque::zmq::{DealerSocket, ReqSocket};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

#[allow(dead_code)]
const READY: &[u8] = b"\x01";
#[allow(dead_code)]
const HEARTBEAT: &[u8] = b"\x02";
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);

/// Worker with heartbeat and crash simulation
#[allow(clippy::future_not_send)]
async fn worker(id: u32, crash_after: Option<u32>) -> std::io::Result<()> {
    info!("[Worker-{}] 🔧 Starting", id);

    let mut socket = DealerSocket::connect("127.0.0.1:5556").await?;

    // Send READY (in real PPP this goes to backend, but we'll log it)
    info!("[Worker-{}] ✅ READY", id);

    let mut heartbeat_timer = Instant::now();
    let mut count = 0u32;

    loop {
        // Heartbeat
        if heartbeat_timer.elapsed() >= HEARTBEAT_INTERVAL {
            info!("[Worker-{}] 💓 HEARTBEAT", id);
            heartbeat_timer = Instant::now();
        }

        // Crash check
        if let Some(crash_at) = crash_after {
            if count >= crash_at {
                error!("[Worker-{}] 💥 CRASH!", id);
                return Ok(());
            }
        }

        // Process requests
        if let Ok(Some(msg)) = socket.recv().await {
            count += 1;
            if let Some(first) = msg.first() {
                info!(
                    "[Worker-{}] 📥 Request #{}: {}",
                    id,
                    count,
                    String::from_utf8_lossy(first)
                );
            }

            compio::runtime::time::sleep(Duration::from_millis(100)).await;

            let reply = format!("Worker-{id} processed request #{count}");
            socket.send(vec![Bytes::from(reply)]).await?;
            info!("[Worker-{}] 📤 Reply #{}", id, count);
        }

        compio::runtime::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Simple client
#[allow(clippy::future_not_send)]
async fn client(id: u32, requests: u32) -> std::io::Result<()> {
    info!("[Client-{}] 🔌 Starting", id);

    compio::runtime::time::sleep(Duration::from_millis(1500)).await;

    let mut socket = ReqSocket::connect("127.0.0.1:5555").await?;

    for i in 1..=requests {
        let req = format!("Request-{i} from Client-{id}");
        info!("[Client-{}] 📨 Sending: {}", id, req);

        socket.send(vec![Bytes::from(req)]).await?;

        if let Ok(Some(reply)) = socket.recv().await {
            if let Some(first) = reply.first() {
                info!(
                    "[Client-{}] 📬 Reply: {}",
                    id,
                    String::from_utf8_lossy(first)
                );
            }
        } else {
            warn!("[Client-{}] ⚠️  No reply", id);
        }

        compio::runtime::time::sleep(Duration::from_millis(800)).await;
    }

    info!("[Client-{}] ✅ Done", id);
    Ok(())
}

/// Simple broker (just forwards, doesn't track heartbeats)
#[allow(clippy::future_not_send)]
async fn broker() -> std::io::Result<()> {
    use futures::{select, FutureExt};

    info!("🚀 Starting Simple Broker");

    let (_, mut frontend) = monocoque::zmq::RouterSocket::bind("127.0.0.1:5555").await?;
    let (_, mut backend) = DealerSocket::bind("127.0.0.1:5556").await?;

    info!("📡 Frontend: 127.0.0.1:5555");
    info!("📡 Backend: 127.0.0.1:5556\n");

    loop {
        select! {
            msg = frontend.recv().fuse() => {
                if let Ok(Some(m)) = msg {
                    backend.send(m).await?;
                }
            }
            msg = backend.recv().fuse() => {
                if let Ok(Some(m)) = msg {
                    frontend.send(m).await?;
                }
            }
        }
    }
}

#[compio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🎬 Paranoid Pirate Pattern - Simple Demo");
    info!("==========================================\n");

    // Broker
    compio::runtime::spawn(async {
        if let Err(e) = broker().await {
            error!("Broker: {}", e);
        }
    })
    .detach();

    compio::runtime::time::sleep(Duration::from_millis(500)).await;

    // Workers
    compio::runtime::spawn(async {
        let _ = worker(1, None).await;
    })
    .detach();
    compio::runtime::time::sleep(Duration::from_millis(100)).await;
    compio::runtime::spawn(async {
        let _ = worker(2, Some(3)).await;
    })
    .detach();

    compio::runtime::time::sleep(Duration::from_secs(1)).await;

    // Client
    let c = compio::runtime::spawn(async { client(1, 6).await });

    // Recovery worker
    compio::runtime::time::sleep(Duration::from_secs(4)).await;
    info!("\n🔄 Spawning recovery worker\n");
    compio::runtime::spawn(async {
        let _ = worker(3, None).await;
    })
    .detach();

    let _ = c.await;
    compio::runtime::time::sleep(Duration::from_secs(2)).await;

    info!("\n✅ Demo Complete!");
    info!("Pattern: Workers send READY/HEARTBEAT, broker forwards messages");

    Ok(())
}
