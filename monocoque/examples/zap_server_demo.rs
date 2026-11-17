//! ZAP Server Demo - Custom Authentication Handler
//!
//! Demonstrates how to create and run a custom ZAP authentication handler
//! on the <inproc://zeromq.zap.01> endpoint.
//!
//! This example shows:
//! - Custom `ZapHandler` implementation
//! - Spawning a ZAP server on inproc transport
//! - Authentication with custom logic (e.g., database lookup)
//! - A PLAIN REQ/REP pair authenticated through that handler

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::security::plain::{PlainAuthHandler, StaticPlainHandler};
use monocoque::zmq::security::zap::{ZapMechanism, ZapRequest, ZapResponse};
use monocoque::zmq::security::zap_handler::{ZapHandler, spawn_zap_server};
use monocoque::zmq::{RepSocket, ReqSocket, Result, SocketOptions};
use std::sync::Arc;
use std::time::Duration;

/// Custom authentication handler that implements business logic
struct CustomZapHandler {
    /// Allowed IP addresses
    allowed_ips: Vec<String>,
    /// PLAIN authentication handler
    plain_handler: Arc<StaticPlainHandler>,
}

impl ZapHandler for CustomZapHandler {
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        println!("ZAP Authentication Request:");
        println!("  Mechanism: {:?}", request.mechanism);
        println!("  Domain: {}", request.domain);
        println!("  Address: {}", request.address);
        println!("  Identity: {} bytes", request.identity.len());

        // The reply has to carry the request's id back.
        let id = request.request_id.as_str();

        // IP whitelist check
        if !self.allowed_ips.contains(&request.address) {
            println!("  DENY: IP {} not in whitelist", request.address);
            return ZapResponse::failure(id, "IP not allowed");
        }

        // Domain check
        if request.domain != "production" && request.domain != "global" {
            println!("  DENY: Invalid domain: {}", request.domain);
            return ZapResponse::failure(id, "Invalid domain");
        }

        // Mechanism-specific authentication. Only PLAIN and CURVE peers get
        // here: a NULL socket never asks the handler.
        match &request.mechanism {
            ZapMechanism::Plain => {
                let [username, password] = request.credentials.as_slice() else {
                    println!("  DENY: PLAIN: Invalid credentials format");
                    return ZapResponse::failure(id, "Invalid credentials");
                };
                let username = String::from_utf8_lossy(username);
                let password = String::from_utf8_lossy(password);
                println!("  PLAIN: username={username}");

                // Use PLAIN handler for actual authentication
                match self
                    .plain_handler
                    .authenticate(&username, &password, &request.domain, &request.address)
                    .await
                {
                    Ok(user_id) => {
                        println!("  ALLOW: PLAIN: Authentication successful");
                        ZapResponse::success(id, user_id)
                    }
                    Err(err) => {
                        println!("  DENY: PLAIN: Authentication failed - {err}");
                        ZapResponse::failure(id, err)
                    }
                }
            }
            ZapMechanism::Curve => {
                let valid_key = matches!(request.credentials.as_slice(), [key] if key.len() == 32);
                if !valid_key {
                    println!("  DENY: CURVE: Invalid public key");
                    return ZapResponse::failure(id, "Invalid CURVE key");
                }

                // This admits any client key: all it checks is the length. A
                // real handler compares the key with a list of allowed ones,
                // as curve_demo.rs does with `with_curve_whitelist`.
                println!("  ALLOW: CURVE: any well-formed public key is admitted");
                ZapResponse::success(id, "curve-client")
            }
            // The enum can grow: refuse a mechanism this handler does not know.
            other => {
                println!("  DENY: Unsupported mechanism {other:?}");
                ZapResponse::failure(id, "Unsupported mechanism")
            }
        }
    }
}

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    println!("=== ZAP Server Demo ===\n");

    // Create custom PLAIN handler with user database
    let mut plain_handler = StaticPlainHandler::new();
    plain_handler.add_user("admin", "secret123");
    plain_handler.add_user("user1", "password1");
    plain_handler.add_user("user2", "password2");

    // Create custom ZAP handler with IP whitelist. The request carries the
    // peer's address as an IP, never as a host name.
    let custom_handler = CustomZapHandler {
        allowed_ips: vec!["127.0.0.1".to_string(), "::1".to_string()],
        plain_handler: Arc::new(plain_handler),
    };

    println!("Configuration:");
    println!("  Allowed IPs: {:?}", custom_handler.allowed_ips);
    println!("  Users: admin, user1, user2");
    println!("  Mechanisms: PLAIN (checked against the users), CURVE (any key)");
    println!();

    // Spawn ZAP server on inproc://zeromq.zap.01
    println!("Starting ZAP server on inproc://zeromq.zap.01");
    spawn_zap_server(Arc::new(custom_handler))?;
    println!("ZAP server running\n");

    // A PLAIN server socket sends a ZAP request for every handshake.
    let mut rep = RepSocket::with_options(
        SocketOptions::default()
            .with_plain_server(true)
            .with_zap_domain("production"),
    );
    let endpoint = rep.bind("tcp://127.0.0.1:0").await?;
    println!("REP server with PLAIN bound to {endpoint}\n");

    let server = rt::spawn(async move {
        // The request is sent back as the reply, so it is taken by value.
        let request = rep.recv_owned().await?;
        rep.send(request).await?;
        rep.close().await
    });

    // A client with an unknown password is rejected, so its send times out.
    // It redials until then, and after two failures in a row the server
    // delays each new connection from that address (100 ms, doubling). The
    // timeout is short so the valid client below, on the same address, waits
    // only briefly.
    println!("Client with a wrong password:");
    let mut bad = ReqSocket::with_options(
        SocketOptions::default()
            .with_plain_credentials("user1", "nope")
            .with_send_timeout(Duration::from_millis(500)),
    );
    bad.connect(&endpoint).await?;
    match bad.send("hello").await {
        Ok(()) => println!("  Unexpected: request went through\n"),
        Err(e) => println!("  Rejected ({e})\n"),
    }
    drop(bad);

    println!("Client with valid credentials:");
    let mut good = ReqSocket::with_options(
        SocketOptions::default()
            .with_plain_credentials("user1", "password1")
            .with_recv_timeout(Duration::from_secs(5)),
    );
    good.connect(&endpoint).await?;
    good.send("hello").await?;
    {
        // The reply is only printed, so it stays borrowed from the socket.
        let reply = good.recv().await?;
        println!(
            "  Echo: {}",
            String::from_utf8_lossy(reply.first().unwrap_or_default())
        );
    }
    good.close().await?;

    rt::join(server).await?;
    println!("\nZAP demo complete.");
    Ok(())
}
