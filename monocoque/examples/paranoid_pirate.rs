//! Paranoid Pirate Pattern - Reliable Request-Reply with Heartbeating
//!
//! This example demonstrates the Paranoid Pirate reliability pattern:
//! - Workers send heartbeats and READY messages
//! - Clients make reliable requests, retrying when a reply never comes
//! - Workers can crash and recover
//! - Broker forwards messages and intercepts worker control messages
//!
//! Architecture:
//! ```text
//! Clients (REQ)  ->  ROUTER (frontend)
//!                        |
//!                    Broker
//!                   (forwarding + heartbeat monitoring)
//!                        |
//!                   DEALER (backend)  <-  Workers (DEALER)
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
use futures::{FutureExt, select};
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, MonocoqueError, Msg, ReqSocket, RouterSocket, SocketOptions};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

// Protocol constants
const READY: &[u8] = b"\x01"; // Worker ready signal
const HEARTBEAT: &[u8] = b"\x02"; // Worker heartbeat

// Timing constants
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(2500);

/// Paranoid Pirate Worker with heartbeating
///
/// Workers send:
/// - READY message on startup
/// - HEARTBEAT messages every second
/// - REPLY messages when processing requests
async fn run_worker(worker_id: u32, crash_after: Option<u32>) -> monocoque::zmq::Result<()> {
    info!("[Worker-{}] Starting paranoid worker", worker_id);

    // The receive timeout wakes the worker up to send heartbeats while idle
    let mut socket =
        DealerSocket::with_options(SocketOptions::default().with_recv_timeout(HEARTBEAT_INTERVAL));
    socket.connect("tcp://127.0.0.1:5556").await?;

    // Send READY signal to broker
    socket.send(READY).await?;
    info!("[Worker-{}] Sent READY to broker", worker_id);

    let mut heartbeat_timer = Instant::now();
    let mut request_count = 0u32;

    loop {
        // Send periodic heartbeat
        if heartbeat_timer.elapsed() >= HEARTBEAT_INTERVAL {
            socket.send(HEARTBEAT).await?;
            debug!("[Worker-{}] Sent heartbeat", worker_id);
            heartbeat_timer = Instant::now();
        }

        // Simulate crash after N requests
        if let Some(crash_at) = crash_after
            && request_count >= crash_at
        {
            error!("[Worker-{}] CRASH SIMULATION - worker dying", worker_id);
            return Ok(());
        }

        // Receive a request: [client identity, empty delimiter, body]
        let msg = match socket.recv_owned().await {
            Ok(msg) if msg.len() >= 2 => msg,
            Ok(_) => continue,
            Err(MonocoqueError::TimedOut) => continue,
            Err(e) => return Err(e),
        };

        request_count += 1;
        let request_data = String::from_utf8_lossy(msg.last().unwrap_or_default()).into_owned();

        info!(
            "[Worker-{}] Processing request #{}: {}",
            worker_id, request_count, request_data
        );

        // Simulate work
        rt::sleep(Duration::from_millis(100)).await;

        // Send reply (echo back with worker ID), keeping the routing frames
        let reply_text = format!("Processed by worker-{worker_id}: {request_data}");
        let mut reply: Msg = (0..msg.len() - 1)
            .filter_map(|i| msg.frame_bytes(i))
            .collect();
        reply.push(Bytes::from(reply_text));

        socket.send(reply).await?;
        info!("[Worker-{}] Sent reply #{}", worker_id, request_count);
    }
}

/// Reliable client making requests
///
/// Sends N requests and waits for replies. A request lost with a crashed
/// worker never gets a reply, so after `REQUEST_TIMEOUT` the client drops
/// its socket, reconnects and sends the request again.
async fn run_client(client_id: u32, num_requests: u32) -> monocoque::zmq::Result<()> {
    info!("[Client-{}] Starting", client_id);

    let options = SocketOptions::default().with_recv_timeout(REQUEST_TIMEOUT);
    let mut socket = ReqSocket::with_options(options.clone());
    socket.connect("tcp://127.0.0.1:5555").await?;

    for i in 1..=num_requests {
        let request = format!("Request {i} from client-{client_id}");
        loop {
            info!("[Client-{}] Sending: {}", client_id, request);
            socket.send(request.clone()).await?;

            // Wait for reply (REQ/REP guarantees request-reply pairs)
            match socket.recv().await {
                Ok(reply) => {
                    info!(
                        "[Client-{}] Received: {:?}",
                        client_id,
                        String::from_utf8_lossy(reply.first().unwrap_or_default())
                    );
                    break;
                }
                Err(MonocoqueError::TimedOut) => {
                    warn!("[Client-{}] No reply received, retrying", client_id);
                    socket = ReqSocket::with_options(options.clone());
                    socket.connect("tcp://127.0.0.1:5555").await?;
                }
                Err(e) => return Err(e),
            }
        }

        // Delay between requests
        rt::sleep(Duration::from_millis(800)).await;
    }

    info!("[Client-{}] Completed all requests", client_id);
    Ok(())
}

/// Broker forwarding between clients and workers
///
/// Requests go from the frontend to the backend, which load-balances them
/// over the connected workers. Replies go back to the frontend, which routes
/// them by the client identity in the first frame. Worker READY and
/// HEARTBEAT messages are intercepted here instead of forwarded.
async fn run_broker() -> monocoque::zmq::Result<()> {
    info!("Starting Paranoid Pirate Broker");

    // Frontend: ROUTER for clients (REQ sockets)
    let mut frontend = RouterSocket::new();
    frontend.bind("tcp://127.0.0.1:5555").await?;
    // Backend: DEALER for workers (DEALER sockets)
    let mut backend = DealerSocket::new();
    backend.bind("tcp://127.0.0.1:5556").await?;

    info!("Frontend (clients) listening on 127.0.0.1:5555");
    info!("Backend (workers) listening on 127.0.0.1:5556\n");

    let mut last_heartbeat = Instant::now();
    loop {
        let (msg, from_client): (Msg, bool) = select! {
            m = frontend.recv_owned().fuse() => (m?, true),
            m = backend.recv_owned().fuse() => (m?, false),
        };
        if from_client {
            backend.send(msg).await?;
        } else if msg == [READY] {
            info!("[Broker] Worker ready ({} connected)", backend.peer_count());
        } else if msg == [HEARTBEAT] {
            debug!(
                "[Broker] Heartbeat, {:?} since the previous one",
                last_heartbeat.elapsed()
            );
            last_heartbeat = Instant::now();
        } else {
            frontend.send(msg).await?;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("Paranoid Pirate Pattern Demo");
    info!("=================================");
    info!("Demonstrating:");
    info!("  - Workers with heartbeating");
    info!("  - READY signals on startup");
    info!("  - Request-reply through broker");
    info!("  - Worker crash simulation");
    info!("  - Recovery worker spawning");
    info!("=================================\n");
    info!("Note: This is a simplified demo.");
    info!("Production brokers would maintain worker queues.\n");

    LocalRuntime::new()?.block_on(async {
        // Spawn broker
        rt::spawn_detached(async {
            if let Err(e) = run_broker().await {
                error!("Broker error: {}", e);
            }
        });

        // Spawn stable worker
        rt::spawn_detached(async {
            if let Err(e) = run_worker(1, None).await {
                error!("Worker 1 error: {}", e);
            }
        });

        // Spawn worker that crashes after 3 requests
        rt::spawn_detached(async {
            if let Err(e) = run_worker(2, Some(3)).await {
                error!("Worker 2 error: {}", e);
            }
        });

        // Wait for workers to connect and send READY
        rt::sleep(Duration::from_millis(500)).await;

        // Spawn client making 6 requests
        let client1 = rt::spawn(async {
            if let Err(e) = run_client(1, 6).await {
                error!("Client 1 error: {}", e);
            }
        });

        // After a few seconds, spawn recovery worker (after worker 2 crashes)
        rt::sleep(Duration::from_secs(4)).await;
        info!("\nSpawning recovery worker...\n");

        rt::spawn_detached(async {
            if let Err(e) = run_worker(3, None).await {
                error!("Worker 3 error: {}", e);
            }
        });

        // Wait for client to finish all requests
        rt::join(client1).await;

        info!("\nDemo completed successfully!");
        info!("\nPattern Summary:");
        info!("  - Workers sent READY signals on connect");
        info!("  - Workers sent heartbeats every second");
        info!("  - Worker 2 crashed after 3 requests");
        info!("  - Worker 3 joined as recovery worker");
        info!("  - All client requests were processed");
        info!("\nFor production:");
        info!("  - Track worker liveness timestamps per worker");
        info!("  - Remove dead workers from queue");
        info!("  - Route requests to available workers");
        info!("  - Use a ROUTER backend to address workers individually");

        Ok(())
    })
}
