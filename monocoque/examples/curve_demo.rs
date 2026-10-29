//! CURVE Encryption Example
//!
//! Demonstrates public-key encryption using the CURVE mechanism (`CurveZMQ`).
//!
//! ## Security Features
//!
//! - **Public-key authentication**: Server verifies client identity
//! - **Perfect forward secrecy**: Ephemeral keys for each connection
//! - **Authenticated encryption**: every message is sealed with `crypto_box`
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
//!
//! Or run both ends in one process:
//! ```bash
//! cargo run --example curve_demo demo
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use monocoque_zmtp::security::curve::{CURVE_KEY_SIZE, CurveSecretKey};
use std::env;
use std::time::Duration;
use tracing::{error, info};

const SERVER_ENDPOINT: &str = "tcp://127.0.0.1:5556";
const REQUESTS: usize = 3;

fn main() {
    LocalRuntime::new().unwrap().block_on(async_main());
}

async fn async_main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!(
            "  {} keygen                          # Generate key pairs",
            args[0]
        );
        eprintln!("  {} server <server_secret_key_hex>  # Run server", args[0]);
        eprintln!("  {} client <server_public_key_hex>  # Run client", args[0]);
        eprintln!("  {} demo                            # Run both", args[0]);
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
            let Some(secret) = parse_key(&args[2]) else {
                return;
            };
            let mut socket = bind_server(secret).await;
            serve(&mut socket, usize::MAX).await;
        }
        "client" => {
            if args.len() != 3 {
                eprintln!("Client requires server public key");
                eprintln!("Usage: {} client <server_public_key_hex>", args[0]);
                std::process::exit(1);
            }
            let Some(server_public) = parse_key(&args[2]) else {
                return;
            };
            run_client(server_public).await;
        }
        "demo" => {
            let (server_public, server_secret) = new_keypair();
            let mut socket = bind_server(server_secret).await;
            let server = rt::spawn(async move {
                serve(&mut socket, REQUESTS).await;
                socket.close().await.ok();
            });
            run_client(server_public).await;
            rt::join(server).await;
        }
        _ => {
            eprintln!("Unknown command. Use 'keygen', 'server', 'client' or 'demo'");
            std::process::exit(1);
        }
    }
}

/// A fresh CURVE key pair. The secret key is 32 random bytes from the OS
/// generator; the public key is derived from it.
fn new_keypair() -> ([u8; CURVE_KEY_SIZE], [u8; CURVE_KEY_SIZE]) {
    use std::io::Read;
    let mut secret = [0u8; CURVE_KEY_SIZE];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut secret))
        .expect("failed to read /dev/urandom");
    let public = *CurveSecretKey::from_bytes(secret).public_key().as_bytes();
    (public, secret)
}

fn parse_key(hex_key: &str) -> Option<[u8; CURVE_KEY_SIZE]> {
    match hex::decode(hex_key) {
        Ok(bytes) if bytes.len() == CURVE_KEY_SIZE => {
            let mut key = [0u8; CURVE_KEY_SIZE];
            key.copy_from_slice(&bytes);
            Some(key)
        }
        Ok(bytes) => {
            error!("Invalid key size: {} bytes (expected 32)", bytes.len());
            None
        }
        Err(e) => {
            error!("Invalid hex key: {}", e);
            None
        }
    }
}

fn generate_keys() {
    info!("Generating CURVE key pairs...\n");

    let (server_public, server_secret) = new_keypair();
    info!("Server Keys:");
    info!("   Public:  {}", hex::encode(server_public));
    info!(
        "   Secret:  {} (keep secure!)\n",
        hex::encode(server_secret)
    );

    let (client_public, _) = new_keypair();
    info!("Client Keys:");
    info!("   Public:  {}", hex::encode(client_public));
    info!("   Secret:  <generated - keep secure!>\n");

    info!("Usage:");
    info!("   1. Save server secret key securely");
    info!("   2. Share server PUBLIC key with clients");
    info!("   3. Run server: cargo run --example curve_demo server <server_secret>");
    info!("   4. Run client: cargo run --example curve_demo client <server_public>");
}

async fn bind_server(secret_array: [u8; CURVE_KEY_SIZE]) -> RepSocket {
    info!("Starting CURVE-encrypted server on {}", SERVER_ENDPOINT);

    // The public key is derived from the secret one
    let public = CurveSecretKey::from_bytes(secret_array).public_key();
    info!("Server public key: {}", hex::encode(public.as_bytes()));

    // Create server socket with CURVE server mode. Every peer that connects
    // to this endpoint has to complete the CURVE handshake.
    let options = SocketOptions::new()
        .with_curve_server(true)
        .with_curve_keypair(*public.as_bytes(), secret_array);

    let mut socket = RepSocket::with_options(options);
    socket.bind(SERVER_ENDPOINT).await.expect("Failed to bind");

    info!(
        "Server listening for encrypted connections on {}",
        SERVER_ENDPOINT
    );
    socket
}

async fn serve(socket: &mut RepSocket, requests: usize) {
    // Handle encrypted requests
    for i in 1..=requests {
        let response = match socket.recv().await {
            Ok(msg) => {
                let request = String::from_utf8_lossy(msg.first().unwrap_or_default());
                info!("Encrypted request #{}: {}", i, request);
                format!("Encrypted reply: {request}")
            }
            Err(e) => {
                info!("Receive failed: {e}");
                break;
            }
        };
        socket.send(response).await.ok();
        info!("Sent encrypted response #{}", i);
    }

    info!("Server shutting down");
}

async fn run_client(server_public_array: [u8; CURVE_KEY_SIZE]) {
    info!("Connecting with CURVE encryption");

    // Generate client keypair
    let (client_public, client_secret) = new_keypair();
    info!("Client public key: {}", hex::encode(client_public));

    // Create client socket with CURVE client mode
    let options = SocketOptions::new()
        .with_curve_keypair(client_public, client_secret)
        .with_curve_serverkey(server_public_array)
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5));

    // Connect to server; the handshake runs as soon as the TCP link is up
    let mut socket = ReqSocket::with_options(options);
    socket
        .connect(SERVER_ENDPOINT)
        .await
        .expect("Failed to connect");

    // Send encrypted requests
    for i in 1..=REQUESTS {
        let message = format!("Secure message #{i}");
        info!("Sending encrypted: {}", message);

        if let Err(e) = socket.send(message).await {
            error!("Send error: {}", e);
            return;
        }

        match socket.recv().await {
            Ok(msg) => {
                let response = String::from_utf8_lossy(msg.first().unwrap_or_default());
                info!("Decrypted response: {}", response);
            }
            Err(e) => {
                error!("Receive error: {e}");
                return;
            }
        }

        rt::sleep(Duration::from_millis(100)).await;
    }

    info!("Client finished successfully");
}
