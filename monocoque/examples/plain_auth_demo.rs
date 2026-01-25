//! PLAIN Authentication Example
//!
//! Demonstrates username/password authentication using the PLAIN mechanism.
//!
//! ## Security Warning
//!
//! PLAIN sends credentials in cleartext! Only use over:
//! - Loopback/localhost connections
//! - Encrypted transports (TLS, VPN, SSH tunnel)
//! - Trusted networks
//!
//! For production over untrusted networks, use CURVE encryption.
//!
//! ## Running
//!
//! Terminal 1 (server):
//! ```bash
//! cargo run --example plain_auth_demo server
//! ```
//!
//! Terminal 2 (client with valid credentials):
//! ```bash
//! cargo run --example plain_auth_demo client admin secret123
//! ```
//!
//! Terminal 3 (client with invalid credentials):
//! ```bash
//! cargo run --example plain_auth_demo client hacker wrongpass
//! ```

use bytes::Bytes;
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use monocoque_zmtp::security::plain::{PlainAuthHandler, PlainCredentials, StaticPlainHandler};
use compio::net::TcpListener;
use std::env;
use std::time::Duration;
use tracing::{error, info};

const SERVER_ADDR: &str = "127.0.0.1:5555";

#[compio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  {} server", args[0]);
        eprintln!("  {} client <username> <password>", args[0]);
        std::process::exit(1);
    }

    match args[1].as_str() {
        "server" => run_server().await,
        "client" => {
            if args.len() != 4 {
                eprintln!("Client requires username and password");
                eprintln!("Usage: {} client <username> <password>", args[0]);
                std::process::exit(1);
            }
            run_client(&args[2], &args[3]).await
        }
        _ => {
            eprintln!("Unknown command. Use 'server' or 'client'");
            std::process::exit(1);
        }
    }
}

async fn run_server() {
    info!("🔐 Starting PLAIN authentication server on {}", SERVER_ADDR);

    // Create authentication handler with valid credentials
    let mut auth_handler = StaticPlainHandler::new();
    auth_handler.add_user("admin", "secret123");
    auth_handler.add_user("guest", "guest123");
    
    info!("✅ Valid credentials:");
    info!("   - admin:secret123");
    info!("   - guest:guest123");

    // Create server socket with PLAIN server mode enabled
    let options = SocketOptions::new()
        .with_plain_server(true)
        .with_zap_domain("example");

    let mut socket = RepSocket::with_options(options);
    
    if let Err(e) = socket.bind(&format!("tcp://{}", SERVER_ADDR)).await {
        error!("❌ Failed to bind: {}", e);
        return;
    }

    info!("🎧 Server listening, waiting for authenticated clients...");

    // Handle requests
    for i in 1..=10 {
        match socket.recv().await {
            Ok(Some(msg)) => {
                let request = String::from_utf8_lossy(&msg[0]);
                info!("📨 Request #{}: {}", i, request);

                // Echo back with prefix
                let response = format!("Server says: {}", request);
                socket.send(vec![Bytes::from(response)]).await.ok();
                
                info!("📤 Sent response #{}", i);
            }
            Ok(None) => {
                info!("ℹ️  Empty message received");
            }
            Err(e) => {
                error!("❌ Receive error: {}", e);
                break;
            }
        }
    }

    info!("👋 Server shutting down");
}

async fn run_client(username: &str, password: &str) {
    info!("🔐 Connecting as user: {}", username);

    // Create client credentials
    let credentials = PlainCredentials::new(username, password);

    // Create client socket with PLAIN credentials
    let options = SocketOptions::new()
        .with_plain_credentials(username, password)
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5));

    let mut socket = ReqSocket::with_options(options);

    if let Err(e) = socket.connect(&format!("tcp://{}", SERVER_ADDR)).await {
        error!("❌ Connection failed: {}", e);
        return;
    }

    info!("✅ Connected to server");

    // Send requests
    for i in 1..=3 {
        let message = format!("Hello from {} (message {})", username, i);
        info!("📤 Sending: {}", message);

        if let Err(e) = socket.send(vec![Bytes::from(message)]).await {
            error!("❌ Send error: {}", e);
            return;
        }

        match socket.recv().await {
            Ok(Some(msg)) => {
                let response = String::from_utf8_lossy(&msg[0]);
                info!("📨 Response: {}", response);
            }
            Ok(None) => {
                error!("❌ Empty response");
                return;
            }
            Err(e) => {
                error!("❌ Receive error: {}", e);
                return;
            }
        }

        // Small delay between messages
        compio::time::sleep(Duration::from_millis(500)).await;
    }

    info!("✅ Client finished successfully");
}
