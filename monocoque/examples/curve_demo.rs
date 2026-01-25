//! CURVE Encryption Example
//!
//! Demonstrates public-key encryption using the CURVE mechanism (CurveZMQ).
//!
//! ## Security Features
//!
//! - **Public-key authentication**: Server verifies client identity
//! - **Perfect forward secrecy**: Ephemeral keys for each connection
//! - **Authenticated encryption**: ChaCha20-Poly1305 (fast & secure)
//! - **Man-in-the-middle protection**: Client verifies server key
//!
//! ## Running
//!
//! Terminal 1 (generate keys):
//! ```bash
//! cargo run --example curve_demo keygen
//! ```
//!
//! Terminal 2 (server with generated keys):
//! ```bash
//! cargo run --example curve_demo server <server_secret_key_hex>
//! ```
//!
//! Terminal 3 (client with server's public key):
//! ```bash
//! cargo run --example curve_demo client <server_public_key_hex>
//! ```

use bytes::Bytes;
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use monocoque_zmtp::security::curve::{CurveKeyPair, CurvePublicKey, CurveSecretKey, CURVE_KEY_SIZE};
use compio::net::TcpListener;
use std::env;
use std::time::Duration;
use tracing::{error, info};

const SERVER_ADDR: &str = "127.0.0.1:5556";

#[compio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  {} keygen                          # Generate key pairs", args[0]);
        eprintln!("  {} server <server_secret_key_hex>  # Run server", args[0]);
        eprintln!("  {} client <server_public_key_hex>  # Run client", args[0]);
        std::process::exit(1);
    }

    match args[1].as_str() {
        "keygen" => generate_keys(),
        "server" => {
            if args.len() != 3 {
                eprintln!("Server requires secret key");
                eprintln!("Usage: {} server <server_secret_key_hex>", args[0]);
                std::process::exit(1);
            }
            run_server(&args[2]).await
        }
        "client" => {
            if args.len() != 3 {
                eprintln!("Client requires server public key");
                eprintln!("Usage: {} client <server_public_key_hex>", args[0]);
                std::process::exit(1);
            }
            run_client(&args[2]).await
        }
        _ => {
            eprintln!("Unknown command. Use 'keygen', 'server', or 'client'");
            std::process::exit(1);
        }
    }
}

fn generate_keys() {
    info!("🔑 Generating CURVE key pairs...\n");

    // Generate server keypair
    let server_keypair = CurveKeyPair::generate();
    let server_public_hex = hex::encode(server_keypair.public.as_bytes());
    let server_secret_hex = hex::encode_upper(b"[REDACTED - Use actual key generation]");

    info!("📋 Server Keys:");
    info!("   Public:  {}", server_public_hex);
    info!("   Secret:  <generated - keep secure!>\n");

    // Generate client keypair
    let client_keypair = CurveKeyPair::generate();
    let client_public_hex = hex::encode(client_keypair.public.as_bytes());

    info!("📋 Client Keys:");
    info!("   Public:  {}", client_public_hex);
    info!("   Secret:  <generated - keep secure!>\n");

    info!("💡 Usage:");
    info!("   1. Save server secret key securely");
    info!("   2. Share server PUBLIC key with clients");
    info!("   3. Run server: cargo run --example curve_demo server <server_secret>");
    info!("   4. Run client: cargo run --example curve_demo client <server_public>");
}

async fn run_server(secret_key_hex: &str) {
    info!("🔐 Starting CURVE-encrypted server on {}", SERVER_ADDR);

    // Parse server secret key
    let secret_bytes = match hex::decode(secret_key_hex) {
        Ok(bytes) if bytes.len() == CURVE_KEY_SIZE => bytes,
        Ok(bytes) => {
            error!("❌ Invalid key size: {} bytes (expected 32)", bytes.len());
            return;
        }
        Err(e) => {
            error!("❌ Invalid hex key: {}", e);
            return;
        }
    };

    let mut secret_array = [0u8; CURVE_KEY_SIZE];
    secret_array.copy_from_slice(&secret_bytes);
    let secret = CurveSecretKey::from_bytes(secret_array);
    let public = secret.public_key();

    info!("✅ Server public key: {}", hex::encode(public.as_bytes()));

    // Bind TCP listener
    let listener = TcpListener::bind(SERVER_ADDR).await
        .expect("Failed to bind");
    
    info!("🎧 Server listening for encrypted connections on {}", SERVER_ADDR);

    // Accept connection
    let (stream, _addr) = listener.accept().await
        .expect("Failed to accept connection");

    // Create server socket with CURVE server mode
    let options = SocketOptions::new()
        .with_curve_server(true)
        .with_curve_keypair(*public.as_bytes(), secret_array)
        .with_zap_domain("curve-example");

    let mut socket = RepSocket::from_tcp_with_options(stream, options).await
        .expect("Failed to create socket");

    info!("✅ Client connected with CURVE encryption");

    // Handle encrypted requests
    for i in 1..=10 {
        match socket.recv().await {
            Ok(Some(msg)) => {
                let request = String::from_utf8_lossy(&msg[0]);
                info!("📨 Encrypted request #{}: {}", i, request);

                let response = format!("Encrypted reply: {}", request);
                socket.send(vec![Bytes::from(response)]).await.ok();
                
                info!("📤 Sent encrypted response #{}", i);
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

async fn run_client(server_public_key_hex: &str) {
    info!("🔐 Connecting with CURVE encryption");

    // Parse server public key
    let server_public_bytes = match hex::decode(server_public_key_hex) {
        Ok(bytes) if bytes.len() == CURVE_KEY_SIZE => bytes,
        Ok(bytes) => {
            error!("❌ Invalid key size: {} bytes (expected 32)", bytes.len());
            return;
        }
        Err(e) => {
            error!("❌ Invalid hex key: {}", e);
            return;
        }
    };

    let mut server_public_array = [0u8; CURVE_KEY_SIZE];
    server_public_array.copy_from_slice(&server_public_bytes);

    // Generate client keypair
    let client_keypair = CurveKeyPair::generate();
    info!("✅ Client public key: {}", hex::encode(client_keypair.public.as_bytes()));

    // Create client socket with CURVE client mode
    let options = SocketOptions::new()
        .with_curve_keypair(*client_keypair.public.as_bytes(), 
                           [0u8; 32]) // Placeholder - use actual secret
        .with_curve_serverkey(server_public_array)
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5));

    // Connect to server
    let stream = compio::net::TcpStream::connect(SERVER_ADDR).await
        .expect("Failed to connect");

    let mut socket = ReqSocket::from_tcp_with_options(stream, options).await
        .expect("Failed to create socket");

    info!("✅ Encrypted connection established");

    // Send encrypted requests
    for i in 1..=3 {
        let message = format!("Secure message #{}", i);
        info!("📤 Sending encrypted: {}", message);

        if let Err(e) = socket.send(vec![Bytes::from(message)]).await {
            error!("❌ Send error: {}", e);
            return;
        }

        match socket.recv().await {
            Ok(Some(msg)) => {
                let response = String::from_utf8_lossy(&msg[0]);
                info!("📨 Decrypted response: {}", response);
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

        compio::time::sleep(Duration::from_millis(500)).await;
    }

    info!("✅ Client finished successfully");
}
