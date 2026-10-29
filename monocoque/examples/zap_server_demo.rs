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

use monocoque::zmq::{RepSocket, ReqSocket, Result, SocketOptions};
use monocoque_zmtp::security::plain::{PlainAuthHandler, StaticPlainHandler};
use monocoque_zmtp::security::zap::{ZapMechanism, ZapRequest, ZapResponse, ZapStatus};
use monocoque_zmtp::security::zap_handler::{ZapHandler, spawn_zap_server};
use std::collections::HashMap;
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
    #[allow(clippy::too_many_lines)]
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        println!("ZAP Authentication Request:");
        println!("  Mechanism: {:?}", request.mechanism);
        println!("  Domain: {}", request.domain);
        println!("  Address: {}", request.address);
        println!("  Identity: {} bytes", request.identity.len());

        // IP whitelist check
        if !self.allowed_ips.contains(&request.address) {
            println!("  DENY: IP {} not in whitelist", request.address);
            return ZapResponse {
                version: "1.0".to_string(),
                request_id: request.request_id.clone(),
                status_code: ZapStatus::Failure,
                status_text: "IP not allowed".to_string(),
                user_id: String::new(),
                metadata: HashMap::new(),
            };
        }

        // Domain check
        if request.domain != "production" && request.domain != "global" {
            println!("  DENY: Invalid domain: {}", request.domain);
            return ZapResponse {
                version: "1.0".to_string(),
                request_id: request.request_id.clone(),
                status_code: ZapStatus::Failure,
                status_text: "Invalid domain".to_string(),
                user_id: String::new(),
                metadata: HashMap::new(),
            };
        }

        // Mechanism-specific authentication
        match request.mechanism {
            ZapMechanism::Null => {
                println!("  ALLOW: NULL mechanism - auto-accept");
                ZapResponse {
                    version: "1.0".to_string(),
                    request_id: request.request_id.clone(),
                    status_code: ZapStatus::Success,
                    status_text: "OK".to_string(),
                    user_id: "anonymous".to_string(),
                    metadata: HashMap::new(),
                }
            }
            ZapMechanism::Plain => {
                if request.credentials.len() != 2 {
                    println!("  DENY: PLAIN: Invalid credentials format");
                    return ZapResponse {
                        version: "1.0".to_string(),
                        request_id: request.request_id.clone(),
                        status_code: ZapStatus::Failure,
                        status_text: "Invalid credentials".to_string(),
                        user_id: String::new(),
                        metadata: HashMap::new(),
                    };
                }

                let username = String::from_utf8_lossy(&request.credentials[0]);
                let password = String::from_utf8_lossy(&request.credentials[1]);

                println!("  PLAIN: username={username}");

                // Use PLAIN handler for actual authentication
                match self
                    .plain_handler
                    .authenticate(&username, &password, &request.domain, &request.address)
                    .await
                {
                    Ok(user_id) => {
                        println!("  ALLOW: PLAIN: Authentication successful");
                        ZapResponse {
                            version: "1.0".to_string(),
                            request_id: request.request_id.clone(),
                            status_code: ZapStatus::Success,
                            status_text: "OK".to_string(),
                            user_id,
                            metadata: HashMap::new(),
                        }
                    }
                    Err(err) => {
                        println!("  DENY: PLAIN: Authentication failed - {err}");
                        ZapResponse {
                            version: "1.0".to_string(),
                            request_id: request.request_id.clone(),
                            status_code: ZapStatus::Failure,
                            status_text: err,
                            user_id: String::new(),
                            metadata: HashMap::new(),
                        }
                    }
                }
            }
            ZapMechanism::Curve => {
                if request.credentials.len() != 1 || request.credentials[0].len() != 32 {
                    println!("  DENY: CURVE: Invalid public key");
                    return ZapResponse {
                        version: "1.0".to_string(),
                        request_id: request.request_id.clone(),
                        status_code: ZapStatus::Failure,
                        status_text: "Invalid CURVE key".to_string(),
                        user_id: String::new(),
                        metadata: HashMap::new(),
                    };
                }

                // In production, check public key against allowed keys database
                println!("  ALLOW: CURVE: Public key accepted");
                ZapResponse {
                    version: "1.0".to_string(),
                    request_id: request.request_id.clone(),
                    status_code: ZapStatus::Success,
                    status_text: "OK".to_string(),
                    user_id: "curve-client".to_string(),
                    metadata: HashMap::new(),
                }
            }
        }
    }
}

fn main() -> Result<()> {
    monocoque::rt::LocalRuntime::new()?.block_on(async {
        println!("=== ZAP Server Demo ===\n");

        // Create custom PLAIN handler with user database
        let mut plain_handler = StaticPlainHandler::new();
        plain_handler.add_user("admin", "secret123");
        plain_handler.add_user("user1", "password1");
        plain_handler.add_user("user2", "password2");

        // Create custom ZAP handler with IP whitelist
        let custom_handler = CustomZapHandler {
            allowed_ips: vec![
                "127.0.0.1".to_string(),
                "::1".to_string(),
                "localhost".to_string(),
            ],
            plain_handler: Arc::new(plain_handler),
        };

        println!("Configuration:");
        println!("  Allowed IPs: {:?}", custom_handler.allowed_ips);
        println!("  Users: admin, user1, user2");
        println!("  Mechanisms: NULL, PLAIN, CURVE");
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

        let server = monocoque::rt::spawn(async move {
            let request = rep.recv_owned().await?;
            rep.send(request).await?;
            rep.close().await
        });

        // A client with an unknown password is rejected, so its send times out.
        println!("Client with a wrong password:");
        let mut bad = ReqSocket::with_options(
            SocketOptions::default()
                .with_plain_credentials("user1", "nope")
                .with_send_timeout(Duration::from_secs(1)),
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
        let reply = good.recv_owned().await?;
        println!(
            "  Echo: {}",
            String::from_utf8_lossy(reply.first().unwrap_or_default())
        );

        monocoque::rt::join(server).await?;
        println!("\nZAP demo complete.");
        Ok(())
    })
}
