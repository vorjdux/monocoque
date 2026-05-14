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
//! cargo run --example plain_auth_demo --features zmq server
//! ```
//!
//! Terminal 2 (client with valid credentials):
//! ```bash
//! cargo run --example plain_auth_demo --features zmq client admin secret123
//! ```

use bytes::Bytes;
use compio::net::TcpListener;
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap_handler::start_default_zap_server;
use std::env;
use std::sync::Arc;
use std::time::Duration;

const SERVER_ADDR: &str = "127.0.0.1:5555";

#[compio::main]
async fn main() {
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
    println!("Starting PLAIN authentication server on {}", SERVER_ADDR);

    let mut auth_handler = StaticPlainHandler::new();
    auth_handler.add_user("admin", "secret123");
    auth_handler.add_user("guest", "guest123");

    start_default_zap_server(Arc::new(auth_handler), false)
        .expect("Failed to start ZAP server");

    let listener = TcpListener::bind(SERVER_ADDR).await.expect("Failed to bind");
    println!("Server listening, waiting for clients...");

    let options = SocketOptions::new()
        .with_plain_server(true)
        .with_zap_domain("example");

    let (stream, addr) = listener.accept().await.expect("Failed to accept");
    println!("Connection from {}", addr);

    let mut socket = RepSocket::from_tcp_with_options(stream, options)
        .await
        .expect("Failed to create socket");

    for i in 1..=3 {
        match socket.recv().await {
            Some(msg) => {
                let request = String::from_utf8_lossy(&msg[0]);
                println!("Request #{}: {}", i, request);

                let response = format!("Server says: {}", request);
                socket.send(vec![Bytes::from(response)]).await.ok();
            }
            None => {
                println!("Connection closed");
                break;
            }
        }
    }

    println!("Server shutting down");
}

async fn run_client(username: &str, password: &str) {
    println!("Connecting as user: {}", username);

    let options = SocketOptions::new()
        .with_plain_credentials(username, password)
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5));

    let mut socket = ReqSocket::connect_with_options(SERVER_ADDR, options)
        .await
        .expect("Failed to connect");

    println!("Connected to server");

    for i in 1..=3 {
        let message = format!("Hello from {} (message {})", username, i);
        println!("Sending: {}", message);

        socket.send(vec![Bytes::from(message)]).await.expect("Send failed");

        match socket.recv().await {
            Some(msg) => {
                let response = String::from_utf8_lossy(&msg[0]);
                println!("Response: {}", response);
            }
            None => {
                println!("Empty response");
                return;
            }
        }

        compio::time::sleep(Duration::from_millis(500)).await;
    }

    println!("Client finished");
}
