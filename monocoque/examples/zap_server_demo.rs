//! ZAP Server Demo - Custom Authentication Handler
//!
//! Demonstrates how to create and run a custom ZAP authentication handler
//! on the <inproc://zeromq.zap.01> endpoint.
//!
//! This example shows:
//! - Custom `ZapHandler` implementation
//! - Spawning a ZAP server on inproc transport
//! - Authentication with custom logic (e.g., database lookup)

use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap::{ZapMechanism, ZapRequest, ZapResponse, ZapStatus};
use monocoque_zmtp::security::zap_handler::{spawn_zap_server, ZapHandler};
use std::collections::HashMap;
use std::sync::Arc;

/// Custom authentication handler that implements business logic
struct CustomZapHandler {
    /// Allowed IP addresses
    allowed_ips: Vec<String>,
    /// PLAIN authentication handler
    plain_handler: Arc<dyn monocoque_zmtp::security::plain::PlainAuthHandler + Send + Sync>,
}

#[async_trait::async_trait(?Send)]
impl ZapHandler for CustomZapHandler {
    #[allow(clippy::too_many_lines)]
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        println!("🔐 ZAP Authentication Request:");
        println!("  Mechanism: {:?}", request.mechanism);
        println!("  Domain: {}", request.domain);
        println!("  Address: {}", request.address);
        println!("  Identity: {} bytes", request.identity.len());

        // IP whitelist check
        if !self.allowed_ips.contains(&request.address) {
            println!("  ❌ IP {} not in whitelist", request.address);
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
            println!("  ❌ Invalid domain: {}", request.domain);
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
                println!("  ✅ NULL mechanism - auto-accept");
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
                    println!("  ❌ PLAIN: Invalid credentials format");
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

                println!("  🔑 PLAIN: username={username}");

                // Use PLAIN handler for actual authentication
                match self
                    .plain_handler
                    .authenticate(&username, &password, &request.domain, &request.address)
                    .await
                {
                    Ok(user_id) => {
                        println!("  ✅ PLAIN: Authentication successful");
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
                        println!("  ❌ PLAIN: Authentication failed - {err}");
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
                    println!("  ❌ CURVE: Invalid public key");
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
                println!("  ✅ CURVE: Public key accepted");
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

fn main() {
    compio::runtime::Runtime::new().unwrap().block_on(async {
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

        println!("📋 Configuration:");
        println!("  Allowed IPs: {:?}", custom_handler.allowed_ips);
        println!("  Users: admin, user1, user2");
        println!("  Mechanisms: NULL, PLAIN, CURVE");
        println!();

        // Spawn ZAP server on inproc://zeromq.zap.01
        println!("🚀 Starting ZAP server on inproc://zeromq.zap.01");
        spawn_zap_server(Arc::new(custom_handler)).expect("Failed to start ZAP server");

        println!("✅ ZAP server running");
        println!();
        println!("The ZAP server is now ready to authenticate connections.");
        println!("Server sockets with security enabled will automatically");
        println!("send authentication requests to this endpoint.");
        println!();
        println!("Example usage:");
        println!("  1. Create a REP socket with .with_plain_server(true)");
        println!("  2. Client connects with .with_plain_credentials(user, pass)");
        println!("  3. ZAP server authenticates the connection");
        println!();

        println!("ZAP server started. Exiting demo.");
    });
}
