//! Steerable Proxy Example - Controllable Message Broker
//!
//! This demonstrates a proxy that can be controlled via a control socket.
//! Commands: PAUSE, RESUME, TERMINATE, STATISTICS
//!
//! # Architecture
//!
//! ```text
//! Clients (REQ) → ROUTER (frontend) ⟷ DEALER (backend) → Workers (REP)
//!                          ↕
//!                    Control Socket (PAIR)
//!                          ↕
//!                   Controller (sends commands)
//! ```
//!
//! Run this example and use another terminal to send control commands.

use bytes::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::proxy::proxy_steerable;
use monocoque::zmq::{DealerSocket, ReqSocket, RouterSocket};
use monocoque_zmtp::pair::PairSocket;
use std::time::Duration;
use tracing::{error, info};

/// Simple worker that processes requests
#[allow(clippy::future_not_send)]
async fn worker(id: u32) -> std::io::Result<()> {
    info!("[Worker-{}] Starting", id);

    // Small delay to let broker start
    rt::sleep(Duration::from_millis(500)).await;

    let mut socket = DealerSocket::connect("127.0.0.1:5556").await?;

    loop {
        if let Ok(Some(mut msg)) = socket.recv().await {
            // Skip empty delimiter
            if !msg.is_empty() && msg[0].is_empty() {
                msg.remove(0);
            }

            if let Some(request) = msg.last() {
                info!(
                    "[Worker-{}] Processing: {}",
                    id,
                    String::from_utf8_lossy(request)
                );
            }

            // Simulate work
            rt::sleep(Duration::from_millis(100)).await;

            // Send reply
            let reply = format!("Processed by worker-{id}");
            let mut response = vec![Bytes::new()];
            response.extend(msg[..msg.len().saturating_sub(1)].to_vec());
            response.push(Bytes::from(reply));

            socket.send(response).await?;
        }

        rt::sleep(Duration::from_millis(10)).await;
    }
}

/// Client that sends requests
#[allow(clippy::future_not_send)]
async fn client(id: u32, requests: u32) -> std::io::Result<()> {
    info!("[Client-{}] Starting", id);

    // Wait for broker and workers
    rt::sleep(Duration::from_secs(1)).await;

    let mut socket = ReqSocket::connect("127.0.0.1:5555").await?;

    for i in 1..=requests {
        let request = format!("Request {i} from client-{id}");
        info!("[Client-{}] Sending: {}", id, request);

        socket.send(vec![Bytes::from(request)]).await?;

        if let Ok(Some(reply)) = socket.recv().await
            && let Some(data) = reply.first()
        {
            info!(
                "[Client-{}] Received: {}",
                id,
                String::from_utf8_lossy(data)
            );
        }

        rt::sleep(Duration::from_millis(500)).await;
    }

    info!("[Client-{}] Done", id);
    Ok(())
}

/// Broker with steerable proxy
#[allow(clippy::future_not_send)]
async fn broker() -> std::io::Result<()> {
    info!("🚀 Starting Steerable Broker");

    // Frontend for clients
    let (_, mut frontend) = RouterSocket::bind("127.0.0.1:5555").await?;
    info!("📡 Frontend (clients): 127.0.0.1:5555");

    // Backend for workers
    let (_, mut backend) = DealerSocket::bind("127.0.0.1:5556").await?;
    info!("📡 Backend (workers): 127.0.0.1:5556");

    // Control socket
    let (_, mut control) = PairSocket::bind("127.0.0.1:5557").await?;
    info!("🎮 Control socket: 127.0.0.1:5557");
    info!("   Send commands: PAUSE, RESUME, TERMINATE, STATISTICS\n");

    // Run steerable proxy
    proxy_steerable(
        &mut frontend,
        &mut backend,
        Option::<&mut DealerSocket>::None,
        &mut control,
    )
    .await?;

    Ok(())
}

/// Controller that sends commands to proxy
#[allow(clippy::future_not_send)]
async fn controller() -> std::io::Result<()> {
    info!("[Controller] Starting");

    // Wait for broker to start
    rt::sleep(Duration::from_millis(800)).await;

    let mut control = PairSocket::connect("127.0.0.1:5557").await?;

    // Let some messages flow
    rt::sleep(Duration::from_secs(3)).await;

    // Pause proxy
    info!("\n[Controller] 🛑 Sending PAUSE command\n");
    control.send(vec![Bytes::from("PAUSE")]).await?;

    // Wait while paused
    rt::sleep(Duration::from_secs(2)).await;

    // Resume proxy
    info!("\n[Controller] ▶️  Sending RESUME command\n");
    control.send(vec![Bytes::from("RESUME")]).await?;

    // Let more messages flow
    rt::sleep(Duration::from_secs(3)).await;

    // Get statistics
    info!("\n[Controller] 📊 Sending STATISTICS command\n");
    control.send(vec![Bytes::from("STATISTICS")]).await?;

    rt::sleep(Duration::from_secs(1)).await;

    // Terminate proxy
    info!("\n[Controller] 🛑 Sending TERMINATE command\n");
    control.send(vec![Bytes::from("TERMINATE")]).await?;

    Ok(())
}

fn main() -> std::io::Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🎬 Steerable Proxy Demo");
    info!("========================");
    info!("Demonstrates:");
    info!("  • Steerable proxy with control socket");
    info!("  • PAUSE/RESUME/TERMINATE commands");
    info!("  • STATISTICS reporting");
    info!("========================\n");

    // Start broker (steerable proxy)
    rt::spawn_detached(async {
        if let Err(e) = broker().await {
            error!("Broker: {}", e);
        }
    });

    rt::sleep(Duration::from_millis(500)).await;

    // Start workers
    rt::spawn_detached(async {
        let _ = worker(1).await;
    });
    rt::spawn_detached(async {
        let _ = worker(2).await;
    });

    rt::sleep(Duration::from_millis(500)).await;

    // Start client (sends 10 requests)
    rt::spawn_detached(async {
        let _ = client(1, 10).await;
    });

    // Start controller (sends commands to proxy)
    let controller_task = rt::spawn(async { controller().await });

    // Wait for controller to finish
    let _ = rt::join(controller_task).await;

    rt::sleep(Duration::from_secs(1)).await;

    info!("\n✅ Demo Complete!");
    info!("\nKey Points:");
    info!("  • Proxy can be controlled via control socket");
    info!("  • PAUSE stops forwarding (messages dropped)");
    info!("  • RESUME restarts forwarding");
    info!("  • TERMINATE gracefully stops proxy");
    info!("  • STATISTICS reports message count");

    Ok(())
}
