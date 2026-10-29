//! Paranoid Pirate Pattern - Simple Demo
//!
//! This demonstrates the key concepts of the Paranoid Pirate pattern:
//! - Workers send READY and HEARTBEAT messages
//! - Simple request-reply through broker
//! - Worker crash and recovery
//!
//! Simplified for a single-threaded runtime.

use futures::{FutureExt, select};
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, MonocoqueError, Msg, ReqSocket, RouterSocket, SocketOptions};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(2500);

/// Worker with heartbeat and crash simulation
async fn worker(id: u32, crash_after: Option<u32>) -> monocoque::zmq::Result<()> {
    info!("[Worker-{}] Starting", id);

    // Wake up at least once per heartbeat interval even with no requests
    let mut socket =
        DealerSocket::with_options(SocketOptions::default().with_recv_timeout(HEARTBEAT_INTERVAL));
    socket.connect("tcp://127.0.0.1:5556").await?;

    // Send READY (in real PPP this goes to backend, but we'll log it)
    info!("[Worker-{}] READY", id);

    let mut heartbeat_timer = Instant::now();
    let mut count = 0u32;

    loop {
        // Heartbeat
        if heartbeat_timer.elapsed() >= HEARTBEAT_INTERVAL {
            info!("[Worker-{}] HEARTBEAT", id);
            heartbeat_timer = Instant::now();
        }

        // Crash check
        if let Some(crash_at) = crash_after
            && count >= crash_at
        {
            error!("[Worker-{}] CRASH!", id);
            return Ok(());
        }

        // Process requests: [client identity, empty delimiter, body]
        let msg = match socket.recv_owned().await {
            Ok(msg) => msg,
            Err(MonocoqueError::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        count += 1;
        info!(
            "[Worker-{}] Request #{}: {}",
            id,
            count,
            String::from_utf8_lossy(msg.last().unwrap_or_default())
        );

        rt::sleep(Duration::from_millis(100)).await;

        // Reply with the same envelope so the broker can route it back
        let mut reply: Msg = (0..msg.len() - 1)
            .filter_map(|i| msg.frame_bytes(i))
            .collect();
        reply.push(format!("Worker-{id} processed request #{count}"));
        socket.send(reply).await?;
        info!("[Worker-{}] Reply #{}", id, count);
    }
}

/// Simple client. A request a crashed worker swallowed gets no reply, so
/// after `REQUEST_TIMEOUT` the client reconnects and sends it again.
async fn client(id: u32, requests: u32) -> monocoque::zmq::Result<()> {
    info!("[Client-{}] Starting", id);

    let options = SocketOptions::default().with_recv_timeout(REQUEST_TIMEOUT);
    let mut socket = ReqSocket::with_options(options.clone());
    socket.connect("tcp://127.0.0.1:5555").await?;

    for i in 1..=requests {
        let req = format!("Request-{i} from Client-{id}");
        loop {
            info!("[Client-{}] Sending: {}", id, req);
            socket.send(req.clone()).await?;

            match socket.recv().await {
                Ok(reply) => {
                    info!(
                        "[Client-{}] Reply: {}",
                        id,
                        String::from_utf8_lossy(reply.first().unwrap_or_default())
                    );
                    break;
                }
                Err(MonocoqueError::TimedOut) => {
                    warn!("[Client-{}] No reply, retrying", id);
                    socket = ReqSocket::with_options(options.clone());
                    socket.connect("tcp://127.0.0.1:5555").await?;
                }
                Err(e) => return Err(e),
            }
        }

        rt::sleep(Duration::from_millis(800)).await;
    }

    info!("[Client-{}] Done", id);
    Ok(())
}

/// Simple broker (just forwards, doesn't track heartbeats)
async fn broker() -> monocoque::zmq::Result<()> {
    info!("Starting Simple Broker");

    let mut frontend = RouterSocket::new();
    frontend.bind("tcp://127.0.0.1:5555").await?;
    let mut backend = DealerSocket::new();
    backend.bind("tcp://127.0.0.1:5556").await?;

    info!("Frontend: 127.0.0.1:5555");
    info!("Backend: 127.0.0.1:5556\n");

    loop {
        let (msg, to_backend): (Msg, bool) = select! {
            m = frontend.recv_owned().fuse() => (m?, true),
            m = backend.recv_owned().fuse() => (m?, false),
        };
        if to_backend {
            backend.send(msg).await?;
        } else {
            frontend.send(msg).await?;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("Paranoid Pirate Pattern - Simple Demo");
    info!("==========================================\n");

    // Broker
    rt::spawn_detached(async {
        if let Err(e) = broker().await {
            error!("Broker: {}", e);
        }
    });

    // Workers
    rt::spawn_detached(async {
        let _ = worker(1, None).await;
    });
    rt::spawn_detached(async {
        let _ = worker(2, Some(3)).await;
    });

    rt::sleep(Duration::from_millis(500)).await;

    // Client
    let c = rt::spawn(async { client(1, 6).await });

    // Recovery worker
    rt::sleep(Duration::from_secs(4)).await;
    info!("\nSpawning recovery worker\n");
    rt::spawn_detached(async {
        let _ = worker(3, None).await;
    });

    rt::join(c).await?;

    info!("\nDemo Complete!");
    info!("Pattern: Workers send READY/HEARTBEAT, broker forwards messages");

    Ok(())
}
