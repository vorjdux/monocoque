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
//! Both sides in one process (a valid and an invalid client):
//! ```bash
//! cargo run --example plain_auth_demo --features zmq
//! ```
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

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{RepSocket, ReqSocket, Result, SocketOptions};
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap_handler::start_default_zap_server;
use std::env;
use std::sync::Arc;
use std::time::Duration;

const SERVER_ADDR: &str = "tcp://127.0.0.1:5555";
const REQUESTS: usize = 3;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        None => run_both().await,
        Some("server") => run_server(SERVER_ADDR).await,
        Some("client") => {
            if args.len() != 4 {
                eprintln!("Client requires username and password");
                eprintln!("Usage: {} client <username> <password>", args[0]);
                std::process::exit(1);
            }
            run_client(SERVER_ADDR, &args[2], &args[3]).await
        }
        Some(_) => {
            eprintln!("Usage:");
            eprintln!("  {}", args[0]);
            eprintln!("  {} server", args[0]);
            eprintln!("  {} client <username> <password>", args[0]);
            std::process::exit(1);
        }
    }
}

fn start_zap() -> Result<()> {
    let mut auth_handler = StaticPlainHandler::new();
    auth_handler.add_user("admin", "secret123");
    auth_handler.add_user("guest", "guest123");
    start_default_zap_server(Arc::new(auth_handler), false)?;
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn run_both() -> Result<()> {
    start_zap()?;
    let mut socket = RepSocket::with_options(
        SocketOptions::default()
            .with_plain_server(true)
            .with_zap_domain("example"),
    );
    let endpoint = socket.bind("tcp://127.0.0.1:0").await?;
    println!("Server listening on {endpoint}");
    let server = rt::spawn(async move { serve(socket).await });

    // A rejected client never gets a connection, so its send times out.
    match run_client(&endpoint, "admin", "wrong-password").await {
        Ok(()) => println!("Unexpected: bad credentials were accepted"),
        Err(e) => println!("Bad credentials rejected as expected ({e})"),
    }
    println!();

    run_client(&endpoint, "admin", "secret123").await?;
    rt::join(server).await
}

#[allow(clippy::future_not_send)]
async fn run_server(endpoint: &str) -> Result<()> {
    println!("Starting PLAIN authentication server on {endpoint}");
    start_zap()?;

    let mut socket = RepSocket::with_options(
        SocketOptions::default()
            .with_plain_server(true)
            .with_zap_domain("example"),
    );
    socket.bind(endpoint).await?;
    println!("Server listening, waiting for clients...");
    serve(socket).await
}

#[allow(clippy::future_not_send)]
async fn serve(mut socket: RepSocket) -> Result<()> {
    for i in 1..=REQUESTS {
        let msg = socket.recv().await?;
        let request = String::from_utf8_lossy(msg.first().unwrap_or_default()).into_owned();
        println!("Request #{i}: {request}");

        socket.send(format!("Server says: {request}")).await?;
    }

    socket.close().await?;
    println!("Server shutting down");
    Ok(())
}

#[allow(clippy::future_not_send)]
async fn run_client(endpoint: &str, username: &str, password: &str) -> Result<()> {
    println!("Connecting as user: {username}");

    let options = SocketOptions::default()
        .with_plain_credentials(username, password)
        .with_recv_timeout(Duration::from_secs(2))
        .with_send_timeout(Duration::from_secs(2));

    let mut socket = ReqSocket::with_options(options);
    socket.connect(endpoint).await?;

    for i in 1..=REQUESTS {
        let message = format!("Hello from {username} (message {i})");
        println!("Sending: {message}");
        socket.send(message).await?;

        let msg = socket.recv().await?;
        println!(
            "Response: {}",
            String::from_utf8_lossy(msg.first().unwrap_or_default())
        );

        rt::sleep(Duration::from_millis(100)).await;
    }

    println!("Client finished");
    Ok(())
}
