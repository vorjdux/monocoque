//! Paranoid Pirate Pattern - Complete Demo with ZeroMQ Proxy
//!
//! This demonstrates the Paranoid Pirate reliability pattern using
//! the ZeroMQ proxy() function which now uses futures::select! internally
//! for single-threaded async runtime compatibility.
//!
//! Architecture:
//! ```text
//! Clients (REQ)  →  ROUTER (frontend)  →  PROXY  →  DEALER (backend)  ←  Workers (DEALER)
//!                                          ↕ ↕                              + HEARTBEAT
//!                                      select!                              + READY
//! ```

use bytes::Bytes;
use monocoque::zmq::{DealerSocket, ReqSocket, RouterSocket};
use monocoque::zmq::proxy::proxy;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const READY: &[u8] = b"\x01";
const HEARTBEAT: &[u8] = b"\x02";
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);

/// Worker sends heartbeats and processes requests
async fn worker(id: u32, crash_after: Option<u32>) -> std::io::Result<()> {
    info!("[Worker-{}] 🔧 Starting", id);
    
    // Small delay to let broker start
    compio::runtime::time::sleep(Duration::from_millis(300)).await;
    
    let mut socket = DealerSocket::connect("127.0.0.1:5556").await?;
    
    // Send READY
    socket.send(vec![Bytes::new(), Bytes::from_static(READY)]).await?;
    info!("[Worker-{}] ✅ Sent READY", id);
    
    let mut heartbeat_timer = Instant::now();
    let mut count = 0u32;
    
    loop {
        // Send heartbeats
        if heartbeat_timer.elapsed() >= HEARTBEAT_INTERVAL {
            socket.send(vec![Bytes::new(), Bytes::from_static(HEARTBEAT)]).await?;
            info!("[Worker-{}] 💓 Heartbeat", id);
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
        if let Some(mut msg) = socket.recv().await {
            // Skip empty delimiter
            if !msg.is_empty() && msg[0].is_empty() {
                msg.remove(0);
            }
            
            count += 1;
            info!("[Worker-{}] 📥 Request #{}", id, count);
            
            compio::runtime::time::sleep(Duration::from_millis(100)).await;
            
            let reply = format!("Processed by worker-{}", id);
            let mut response = vec![Bytes::new()];
            response.extend(msg[..msg.len().saturating_sub(1)].to_vec());
            response.push(Bytes::from(reply));
            
            socket.send(response).await?;
            info!("[Worker-{}] 📤 Reply #{}", id, count);
        }
        
        compio::runtime::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Client sends requests
async fn client(id: u32, requests: u32) -> std::io::Result<()> {
    info!("[Client-{}] 🔌 Starting", id);
    
    // Wait for broker and workers
    compio::runtime::time::sleep(Duration::from_secs(2)).await;
    
    let mut socket = ReqSocket::connect("127.0.0.1:5555").await?;
    
    for i in 1..=requests {
        info!("[Client-{}] 📨 Request {}", id, i);
        
        socket.send(vec![Bytes::from(format!("Request {}", i))]).await?;
        
        if let Some(reply) = socket.recv().await {
            if let Some(data) = reply.first() {
                info!("[Client-{}] 📬 {}", id, String::from_utf8_lossy(data));
            }
        } else {
            warn!("[Client-{}] ⚠️  No reply", id);
        }
        
        compio::runtime::time::sleep(Duration::from_millis(900)).await;
    }
    
    info!("[Client-{}] ✅ Done", id);
    Ok(())
}

/// Broker using ZeroMQ proxy with futures::select!
async fn broker() -> std::io::Result<()> {
    info!("🚀 Starting Broker with ZeroMQ Proxy");
    
    // Bind frontend and backend - gets first connection for each
    let (_, mut frontend) = RouterSocket::bind("127.0.0.1:5555").await?;
    let (_, mut backend) = DealerSocket::bind("127.0.0.1:5556").await?;
    
    info!("📡 Frontend (clients): 127.0.0.1:5555");
    info!("📡 Backend (workers): 127.0.0.1:5556");
    info!("🔄 Proxy running with futures::select! (async-aware)\n");
    
    // ZeroMQ proxy - now uses futures::select! internally!
    // Forwards messages bidirectionally: frontend ←→ backend
    proxy(&mut frontend, &mut backend, Option::<&mut DealerSocket>::None).await?;
    
    Ok(())
}

#[compio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🎬 Paranoid Pirate Pattern - ZeroMQ Proxy Demo");
    info!("===============================================");
    info!("Demonstrates:");
    info!("  • ZeroMQ proxy() with futures::select!");
    info!("  • Workers with READY + HEARTBEAT");
    info!("  • Async-aware bidirectional forwarding");
    info!("  • Worker crash and recovery");
    info!("===============================================\n");

    // Start broker (proxy)
    compio::runtime::spawn(async {
        if let Err(e) = broker().await {
            error!("Broker: {}", e);
        }
    }).detach();

    compio::runtime::time::sleep(Duration::from_millis(500)).await;

    // Start workers
    compio::runtime::spawn(async { let _ = worker(1, None).await; }).detach();
    compio::runtime::time::sleep(Duration::from_millis(150)).await;
    compio::runtime::spawn(async { let _ = worker(2, Some(3)).await; }).detach();

    compio::runtime::time::sleep(Duration::from_millis(1000)).await;

    // Start client
    let client_task = compio::runtime::spawn(async { client(1, 6).await });

    // Spawn recovery worker after 5 seconds
    compio::runtime::time::sleep(Duration::from_secs(5)).await;
    info!("\n🔄 Recovery worker joining\n");
    compio::runtime::spawn(async { let _ = worker(3, None).await; }).detach();

    let _ = client_task.await;
    compio::runtime::time::sleep(Duration::from_secs(3)).await;

    info!("\n✅ Demo Complete!");
    info!("\nKey Points:");
    info!("  • proxy() now uses futures::select! internally");
    info!("  • Works correctly in single-threaded compio runtime");
    info!("  • Forwards READY, HEARTBEAT, and request/reply");
    info!("  • Production: intercept control messages for tracking");
    
    Ok(())
}
