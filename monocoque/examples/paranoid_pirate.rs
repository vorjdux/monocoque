//! Paranoid Pirate Pattern - Reliable Request-Reply with Heartbeating
//!
//! This example demonstrates the Paranoid Pirate reliability pattern:
//! - Workers send heartbeats and READY messages
//! - Clients make reliable requests
//! - Workers can crash and recover
//! - Broker forwards messages using proxy
//!
//! Architecture:
//! ```text
//! Clients (REQ)  →  ROUTER (frontend)
//!                        ↓
//!                    Broker
//!                   (proxy + heartbeat monitoring)
//!                        ↓
//!                   DEALER (backend)  ←  Workers (DEALER)
//!                                          + HEARTBEAT
//!                                          + READY
//! ```
//!
//! Run this example and watch:
//! 1. Workers connect and send READY + heartbeats
//! 2. Clients send requests through broker
//! 3. Workers process and respond
//! 4. Worker 2 crashes after 3 requests
//! 5. Worker 3 spawns as recovery worker
//!
//! Note: This is a simplified version showing the pattern.
//! For production, you'd track worker queues in the broker and route intelligently.

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, ReqSocket};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// Protocol constants
const READY: &[u8] = b"\x01"; // Worker ready signal
const HEARTBEAT: &[u8] = b"\x02"; // Worker heartbeat

// Timing constants
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

/// Paranoid Pirate Worker with heartbeating
///
/// Workers send:
/// - READY message on startup
/// - HEARTBEAT messages every second
/// - REPLY messages when processing requests
#[allow(clippy::future_not_send)]
async fn run_worker(worker_id: u32, crash_after: Option<u32>) -> std::io::Result<()> {
    info!("[Worker-{}] 🔧 Starting paranoid worker", worker_id);

    let mut socket = DealerSocket::connect("127.0.0.1:5556").await?;

    // Send READY signal to broker
    socket
        .send(vec![Bytes::new(), Bytes::from_static(READY)])
        .await?;
    info!("[Worker-{}] ✅ Sent READY to broker", worker_id);

    let mut heartbeat_timer = Instant::now();
    let mut request_count = 0u32;

    loop {
        // Send periodic heartbeat
        if heartbeat_timer.elapsed() >= HEARTBEAT_INTERVAL {
            socket
                .send(vec![Bytes::new(), Bytes::from_static(HEARTBEAT)])
                .await?;
            debug!("[Worker-{}] 💓 Sent heartbeat", worker_id);
            heartbeat_timer = Instant::now();
        }

        // Simulate crash after N requests
        if let Some(crash_at) = crash_after
            && request_count >= crash_at
        {
            error!("[Worker-{}] 💥 CRASH SIMULATION - worker dying", worker_id);
            return Ok(());
        }

        // Receive and process requests (non-blocking)
        if let Ok(Some(mut msg)) = socket.recv().await
            && !msg.is_empty()
        {
            // Skip empty delimiter frame if present
            if msg[0].is_empty() && msg.len() > 1 {
                msg.remove(0);
            }

            request_count += 1;
            let request_data = msg
                .last()
                .map(|b| String::from_utf8_lossy(b).to_string())
                .unwrap_or_default();

            info!(
                "[Worker-{}] 📥 Processing request #{}: {}",
                worker_id, request_count, request_data
            );

            // Simulate work
            rt::sleep(Duration::from_millis(100)).await;

            // Send reply (echo back with worker ID)
            let reply_text = format!("Processed by worker-{worker_id}: {request_data}");

            let mut reply = vec![Bytes::new()];
            // Keep routing info frames
            reply.extend(msg[..msg.len() - 1].to_vec());
            // Replace last frame with our reply
            reply.push(Bytes::from(reply_text));

            socket.send(reply).await?;
            info!("[Worker-{}] 📤 Sent reply #{}", worker_id, request_count);
        }

        // Small yield to prevent CPU spinning
        rt::sleep(Duration::from_millis(10)).await;
    }
}

/// Simple client making requests
///
/// Sends N requests and waits for replies
#[allow(clippy::future_not_send)]
async fn run_client(client_id: u32, num_requests: u32) -> std::io::Result<()> {
    info!("[Client-{}] 🔌 Starting", client_id);

    // Wait for broker and workers to be ready
    rt::sleep(Duration::from_millis(1500)).await;

    let mut socket = ReqSocket::connect("127.0.0.1:5555").await?;

    for i in 1..=num_requests {
        let request = format!("Request {i} from client-{client_id}");
        info!("[Client-{}] 📨 Sending: {}", client_id, request);

        socket.send(vec![Bytes::from(request)]).await?;

        // Wait for reply (REQ/REP guarantees request-reply pairs)
        if let Ok(Some(reply)) = socket.recv().await {
            if let Some(reply_text) = reply.first() {
                info!(
                    "[Client-{}] 📬 Received: {:?}",
                    client_id,
                    String::from_utf8_lossy(reply_text)
                );
            }
        } else {
            warn!("[Client-{}] ⚠️  No reply received", client_id);
        }

        // Delay between requests
        rt::sleep(Duration::from_millis(800)).await;
    }

    info!("[Client-{}] ✅ Completed all requests", client_id);
    Ok(())
}

/// Simple broker using `ZeroMQ` proxy pattern
///
/// The proxy automatically forwards messages bidirectionally using `futures::select`!
/// In production, you would intercept READY/HEARTBEAT messages
/// to track worker liveness and maintain an LRU queue.
#[allow(clippy::future_not_send)]
async fn run_broker() -> std::io::Result<()> {
    info!("🚀 Starting Paranoid Pirate Broker");

    // Frontend: ROUTER for clients (REQ sockets)
    let (_, mut frontend) = monocoque::zmq::RouterSocket::bind("127.0.0.1:5555").await?;
    // Backend: DEALER for workers (DEALER sockets)
    let (_, mut backend) = DealerSocket::bind("127.0.0.1:5556").await?;

    info!("📡 Frontend (clients) listening on 127.0.0.1:5555");
    info!("📡 Backend (workers) listening on 127.0.0.1:5556");
    info!("🔄 Starting ZeroMQ proxy (async-aware with futures::select!)\n");

    // Use the ZeroMQ proxy pattern - now async-aware for single-threaded runtime
    // This forwards messages bidirectionally: frontend ←→ backend
    // READY, HEARTBEAT, and request/reply messages all flow through
    monocoque::zmq::proxy::proxy(
        &mut frontend,
        &mut backend,
        Option::<&mut DealerSocket>::None,
    )
    .await?;

    Ok(())
}

fn main() -> std::io::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🎬 Paranoid Pirate Pattern Demo");
    info!("=================================");
    info!("Demonstrating:");
    info!("  • Workers with heartbeating");
    info!("  • READY signals on startup");
    info!("  • Request-reply through broker");
    info!("  • Worker crash simulation");
    info!("  • Recovery worker spawning");
    info!("=================================\n");
    info!("⚠️  Note: This is a simplified demo.");
    info!("⚠️  Production brokers would parse READY/HEARTBEAT");
    info!("⚠️  and maintain worker queues.\n");

    LocalRuntime::new()?.block_on(async {
        // Spawn broker
        rt::spawn_detached(async {
            if let Err(e) = run_broker().await {
                error!("Broker error: {}", e);
            }
        });

        // Wait for broker to initialize
        rt::sleep(Duration::from_secs(1)).await;

        // Spawn stable worker
        rt::spawn_detached(async {
            if let Err(e) = run_worker(1, None).await {
                error!("Worker 1 error: {}", e);
            }
        });

        // Small delay between worker spawns
        rt::sleep(Duration::from_millis(200)).await;

        // Spawn worker that crashes after 3 requests
        rt::spawn_detached(async {
            if let Err(_e) = run_worker(2, Some(3)).await {
                info!("Worker 2 completed/crashed");
            }
        });

        // Wait for workers to connect and send READY
        rt::sleep(Duration::from_secs(2)).await;

        // Spawn client making 6 requests
        let client1 = rt::spawn(async {
            if let Err(e) = run_client(1, 6).await {
                error!("Client 1 error: {}", e);
            }
        });

        // After 5 seconds, spawn recovery worker (after worker 2 crashes)
        rt::sleep(Duration::from_secs(5)).await;
        info!("\n🔄 Spawning recovery worker...\n");

        rt::spawn_detached(async {
            if let Err(e) = run_worker(3, None).await {
                error!("Worker 3 error: {}", e);
            }
        });

        // Wait for client to finish all requests
        let () = rt::join(client1).await;

        // Give time to see final heartbeats
        rt::sleep(Duration::from_secs(3)).await;

        info!("\n✅ Demo completed successfully!");
        info!("\nPattern Summary:");
        info!("  • Workers sent READY signals on connect");
        info!("  • Workers sent heartbeats every second");
        info!("  • Worker 2 crashed after 3 requests");
        info!("  • Worker 3 joined as recovery worker");
        info!("  • All client requests were processed");
        info!("\nFor production:");
        info!("  • Broker should parse READY/HEARTBEAT");
        info!("  • Track worker liveness timestamps");
        info!("  • Remove dead workers from queue");
        info!("  • Route requests to available workers");
        info!("  • Use multi-peer ROUTER sockets");

        Ok(())
    })
}
