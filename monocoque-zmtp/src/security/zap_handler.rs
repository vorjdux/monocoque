/// ZAP (ZeroMQ Authentication Protocol) Handler Infrastructure
///
/// This module provides the ZAP handler infrastructure for authentication.
/// ZAP handlers run on inproc://zeromq.zap.01 and process authentication
/// requests from server sockets.
use crate::security::plain::PlainAuthHandler;
use crate::security::zap::{ZapMechanism, ZapRequest, ZapResponse};
use crate::{inproc_stream::InprocStream, DealerSocket};
use monocoque_core::options::SocketOptions;
use std::io;
use std::sync::Arc;

/// Trait for custom ZAP authentication handlers
///
/// Implement this trait to provide custom authentication logic
/// that handles requests from all security mechanisms.
#[async_trait::async_trait(?Send)]
pub trait ZapHandler {
    /// Process a ZAP authentication request
    ///
    /// # Arguments
    /// * `request` - The ZAP request containing credentials and metadata
    ///
    /// # Returns
    /// A ZAP response with authentication result (200/400/500)
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse;
}

/// Default ZAP handler that uses a PlainAuthHandler for PLAIN mechanism
/// and accepts all CURVE connections with valid keys.
pub struct DefaultZapHandler<H: PlainAuthHandler> {
    plain_handler: Arc<H>,
    accept_curve: bool,
    /// Optional whitelist of permitted CURVE public keys (32 bytes each).
    /// When Some, only listed keys are accepted. When None, all valid keys are accepted.
    curve_key_whitelist: Option<std::collections::HashSet<[u8; 32]>>,
}

impl<H: PlainAuthHandler> DefaultZapHandler<H> {
    /// Create handler. `accept_curve=true` accepts all valid CURVE keys (no whitelist).
    /// Use `with_curve_whitelist()` to restrict to specific keys.
    pub const fn new(plain_handler: Arc<H>, accept_curve: bool) -> Self {
        Self {
            plain_handler,
            accept_curve,
            curve_key_whitelist: None,
        }
    }

    /// Set an explicit whitelist of permitted CURVE public keys.
    /// Only keys in this set will be accepted.
    pub fn with_curve_whitelist(mut self, keys: Vec<[u8; 32]>) -> Self {
        self.curve_key_whitelist = Some(keys.into_iter().collect());
        self
    }
}

#[async_trait::async_trait(?Send)]
impl<H: PlainAuthHandler> ZapHandler for DefaultZapHandler<H> {
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        match request.mechanism {
            ZapMechanism::Null => {
                // NULL mechanism - always accept
                ZapResponse::success(request.request_id.clone(), String::new())
            }
            ZapMechanism::Plain => {
                // Extract username and password
                if request.credentials.len() < 2 {
                    return ZapResponse::failure(request.request_id.clone(), "Missing credentials");
                }

                let username = String::from_utf8_lossy(&request.credentials[0]);
                let password = String::from_utf8_lossy(&request.credentials[1]);

                // Call PLAIN handler
                match self
                    .plain_handler
                    .authenticate(&username, &password, &request.domain, &request.address)
                    .await
                {
                    Ok(user_id) => ZapResponse::success(request.request_id.clone(), user_id),
                    Err(err) => ZapResponse::failure(request.request_id.clone(), &err),
                }
            }
            ZapMechanism::Curve => {
                if !self.accept_curve {
                    return ZapResponse::failure(request.request_id.clone(), "CURVE not enabled");
                }

                // CURVE mechanism - verify public key is present
                if request.credentials.is_empty() {
                    return ZapResponse::failure(
                        request.request_id.clone(),
                        "Missing CURVE public key",
                    );
                }

                let public_key = &request.credentials[0];
                if public_key.len() != 32 {
                    return ZapResponse::failure(
                        request.request_id.clone(),
                        "Invalid CURVE key length",
                    );
                }

                // Check whitelist if configured
                if let Some(ref whitelist) = self.curve_key_whitelist {
                    let mut key_arr = [0u8; 32];
                    key_arr.copy_from_slice(public_key);
                    if !whitelist.contains(&key_arr) {
                        return ZapResponse::failure(
                            request.request_id.clone(),
                            "CURVE key not in whitelist",
                        );
                    }
                }
                // When no whitelist configured: accept all valid keys (accept_curve=true already checked)

                // Use the hex-encoded public key as user_id
                let user_id = public_key
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<String>();
                ZapResponse::success(request.request_id.clone(), user_id)
            }
        }
    }
}

/// ZAP server that runs on inproc://zeromq.zap.01
///
/// This is the standard ZAP endpoint that server sockets send
/// authentication requests to.
pub struct ZapServer<H: ZapHandler> {
    socket: DealerSocket<InprocStream>,
    handler: Arc<H>,
}

impl<H: ZapHandler> ZapServer<H> {
    /// Create a new ZAP server with the given handler
    ///
    /// # Arguments
    /// * `handler` - The ZAP handler to process authentication requests
    ///
    /// # Returns
    /// A new ZAP server that binds to inproc://zeromq.zap.01
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::security::zap_handler::{ZapServer, DefaultZapHandler};
    /// use monocoque_zmtp::security::plain::StaticPlainHandler;
    /// use std::sync::Arc;
    ///
    /// fn run_zap_server() -> std::io::Result<()> {
    ///     // Create a simple PLAIN handler
    ///     let mut plain_handler = StaticPlainHandler::new();
    ///     plain_handler.add_user("admin", "secret");
    ///     let plain_handler = Arc::new(plain_handler);
    ///
    ///     // Create default ZAP handler
    ///     let zap_handler = Arc::new(DefaultZapHandler::new(plain_handler, true));
    ///
    ///     // Create ZAP server (binds immediately)
    ///     let server = ZapServer::new(zap_handler)?;
    ///     Ok(())
    /// }
    /// ```
    pub fn new(handler: Arc<H>) -> io::Result<Self> {
        // Bind to the standard ZAP endpoint
        let socket = DealerSocket::bind_inproc("inproc://zeromq.zap.01", SocketOptions::default())?;

        Ok(Self { socket, handler })
    }

    /// Start the ZAP server
    ///
    /// Processes authentication requests in a loop. This function runs until
    /// an error occurs.
    ///
    /// The server receives ZAP requests, processes them through the handler,
    /// and sends back responses on inproc://zeromq.zap.01.
    pub async fn start(&mut self) -> io::Result<()> {
        loop {
            // Receive ZAP request
            let Some(msg) = self.socket.recv().await? else {
                continue;
            };

            // Decode the request
            let request = match ZapRequest::decode(&msg) {
                Ok(req) => req,
                Err(_e) => {
                    // Failed to decode ZAP request
                    continue;
                }
            };

            // Process the request
            let response = self.handler.authenticate(&request).await;

            // Send the response
            let frames = response.encode();
            if let Err(_e) = self.socket.send(frames).await {
                // Failed to send ZAP response
            }
        }
    }
}

/// Helper to spawn a ZAP server in a background task
///
/// # Arguments
/// * `handler` - The ZAP handler to use
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::security::zap_handler::{spawn_zap_server, DefaultZapHandler};
/// use monocoque_zmtp::security::plain::StaticPlainHandler;
/// use std::sync::Arc;
///
/// fn setup_auth() -> std::io::Result<()> {
///     let mut plain_handler = StaticPlainHandler::new();
///     plain_handler.add_user("admin", "secret");
///     let plain_handler = Arc::new(plain_handler);
///     let zap_handler = Arc::new(DefaultZapHandler::new(plain_handler, true));
///
///     spawn_zap_server(zap_handler)?;
///     Ok(())
/// }
/// ```
pub fn spawn_zap_server<H: ZapHandler + 'static>(handler: Arc<H>) -> io::Result<()> {
    let mut server = ZapServer::new(handler)?;
    compio::runtime::spawn(async move {
        let _ = server.start().await;
    })
    .detach();
    Ok(())
}

/// Convenience function to start a ZAP server with default handler
///
/// # Arguments
/// * `plain_handler` - Handler for PLAIN authentication
/// * `accept_curve` - Whether to accept CURVE connections
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::security::zap_handler::start_default_zap_server;
/// use monocoque_zmtp::security::plain::StaticPlainHandler;
/// use std::sync::Arc;
///
/// fn setup() -> std::io::Result<()> {
///     let mut handler = StaticPlainHandler::new();
///     handler.add_user("admin", "secret");
///     let handler = Arc::new(handler);
///
///     start_default_zap_server(handler, true)?;
///     Ok(())
/// }
/// ```
pub fn start_default_zap_server<H: PlainAuthHandler + 'static>(
    plain_handler: Arc<H>,
    accept_curve: bool,
) -> io::Result<()> {
    let zap_handler = Arc::new(DefaultZapHandler::new(plain_handler, accept_curve));
    spawn_zap_server(zap_handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::plain::StaticPlainHandler;
    use crate::security::ZapStatus;
    use bytes::Bytes;

    #[test]
    fn test_default_zap_handler_null() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let plain_handler = Arc::new(StaticPlainHandler::new());
            let handler = DefaultZapHandler::new(plain_handler, true);

            let request = ZapRequest {
                version: "1.0".to_string(),
                request_id: "1".to_string(),
                domain: "global".to_string(),
                address: "127.0.0.1".to_string(),
                identity: Bytes::new(),
                mechanism: ZapMechanism::Null,
                credentials: vec![],
            };

            let response = handler.authenticate(&request).await;
            assert_eq!(response.status_code, ZapStatus::Success);
        });
    }

    #[test]
    fn test_default_zap_handler_plain_success() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut plain_handler = StaticPlainHandler::new();
            plain_handler.add_user("admin", "secret");
            let handler = DefaultZapHandler::new(Arc::new(plain_handler), true);

            let request = ZapRequest {
                version: "1.0".to_string(),
                request_id: "2".to_string(),
                domain: "global".to_string(),
                address: "127.0.0.1".to_string(),
                identity: Bytes::new(),
                mechanism: ZapMechanism::Plain,
                credentials: vec![Bytes::from("admin"), Bytes::from("secret")],
            };

            let response = handler.authenticate(&request).await;
            assert_eq!(response.status_code, ZapStatus::Success);
            assert_eq!(response.user_id, "admin");
        });
    }

    #[test]
    fn test_default_zap_handler_plain_failure() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let plain_handler = Arc::new(StaticPlainHandler::new());
            let handler = DefaultZapHandler::new(plain_handler, true);

            let request = ZapRequest {
                version: "1.0".to_string(),
                request_id: "3".to_string(),
                domain: "global".to_string(),
                address: "127.0.0.1".to_string(),
                identity: Bytes::new(),
                mechanism: ZapMechanism::Plain,
                credentials: vec![Bytes::from("admin"), Bytes::from("wrong")],
            };

            let response = handler.authenticate(&request).await;
            assert_eq!(response.status_code, ZapStatus::Failure);
        });
    }

    #[test]
    fn test_default_zap_handler_curve_success() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let plain_handler = Arc::new(StaticPlainHandler::new());
            let handler = DefaultZapHandler::new(plain_handler, true);

            let public_key = [0u8; 32];
            let request = ZapRequest {
                version: "1.0".to_string(),
                request_id: "4".to_string(),
                domain: "global".to_string(),
                address: "127.0.0.1".to_string(),
                identity: Bytes::new(),
                mechanism: ZapMechanism::Curve,
                credentials: vec![Bytes::copy_from_slice(&public_key)],
            };

            let response = handler.authenticate(&request).await;
            assert_eq!(response.status_code, ZapStatus::Success);
        });
    }

    #[test]
    fn test_default_zap_handler_curve_disabled() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let plain_handler = Arc::new(StaticPlainHandler::new());
            let handler = DefaultZapHandler::new(plain_handler, false);

            let public_key = [0u8; 32];
            let request = ZapRequest {
                version: "1.0".to_string(),
                request_id: "5".to_string(),
                domain: "global".to_string(),
                address: "127.0.0.1".to_string(),
                identity: Bytes::new(),
                mechanism: ZapMechanism::Curve,
                credentials: vec![Bytes::copy_from_slice(&public_key)],
            };

            let response = handler.authenticate(&request).await;
            assert_eq!(response.status_code, ZapStatus::Failure);
        });
    }

    /// A ZAP handler that rejects connections from a configurable deny-list of
    /// IP addresses by returning a 400 (Failure) response.
    struct IpDenyListHandler {
        denied_ips: Vec<String>,
    }

    impl IpDenyListHandler {
        fn new(denied_ips: Vec<&str>) -> Self {
            Self {
                denied_ips: denied_ips.into_iter().map(str::to_string).collect(),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ZapHandler for IpDenyListHandler {
        async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
            // Reject if the peer address starts with any denied IP prefix
            if self
                .denied_ips
                .iter()
                .any(|ip| request.address.starts_with(ip.as_str()))
            {
                return ZapResponse::failure(
                    request.request_id.clone(),
                    format!("Address {} is blocked", request.address),
                );
            }
            ZapResponse::success(request.request_id.clone(), String::new())
        }
    }

    /// Verify that a ZAP handler which returns 400 for specific IP addresses
    /// causes those addresses to be treated as rejected, while others are accepted.
    #[test]
    fn test_ip_based_rejection() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let handler = IpDenyListHandler::new(vec!["192.168.1.100", "10.0.0.1"]);

            // --- Denied address: should receive a Failure (400) response ---
            let denied_request = ZapRequest {
                version: "1.0".to_string(),
                request_id: "deny-1".to_string(),
                domain: "global".to_string(),
                address: "192.168.1.100".to_string(), // in the deny list
                identity: Bytes::new(),
                mechanism: ZapMechanism::Null,
                credentials: vec![],
            };
            let denied_response = handler.authenticate(&denied_request).await;
            assert_eq!(
                denied_response.status_code,
                ZapStatus::Failure,
                "connections from denied IPs must be rejected with status 400"
            );
            assert!(
                denied_response.status_text.contains("192.168.1.100"),
                "failure message should name the blocked address"
            );

            // --- Another denied address ---
            let denied_request2 = ZapRequest {
                version: "1.0".to_string(),
                request_id: "deny-2".to_string(),
                domain: "global".to_string(),
                address: "10.0.0.1".to_string(), // also in the deny list
                identity: Bytes::new(),
                mechanism: ZapMechanism::Null,
                credentials: vec![],
            };
            let denied_response2 = handler.authenticate(&denied_request2).await;
            assert_eq!(
                denied_response2.status_code,
                ZapStatus::Failure,
                "10.0.0.1 is on the deny list and must be rejected"
            );

            // --- Allowed address: should receive a Success (200) response ---
            let allowed_request = ZapRequest {
                version: "1.0".to_string(),
                request_id: "allow-1".to_string(),
                domain: "global".to_string(),
                address: "127.0.0.1".to_string(), // NOT in the deny list
                identity: Bytes::new(),
                mechanism: ZapMechanism::Null,
                credentials: vec![],
            };
            let allowed_response = handler.authenticate(&allowed_request).await;
            assert_eq!(
                allowed_response.status_code,
                ZapStatus::Success,
                "connections from allowed IPs must succeed with status 200"
            );
        });
    }

    /// Verify that an IP subnet prefix match also blocks sub-addresses correctly.
    #[test]
    fn test_ip_subnet_prefix_rejection() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            // Block the entire 10.0.0.x range using a prefix
            let handler = IpDenyListHandler::new(vec!["10.0.0."]);

            let blocked = ZapRequest {
                version: "1.0".to_string(),
                request_id: "subnet-1".to_string(),
                domain: "global".to_string(),
                address: "10.0.0.55".to_string(),
                identity: Bytes::new(),
                mechanism: ZapMechanism::Null,
                credentials: vec![],
            };
            let resp = handler.authenticate(&blocked).await;
            assert_eq!(
                resp.status_code,
                ZapStatus::Failure,
                "addresses matching a denied subnet prefix must be rejected"
            );

            let allowed = ZapRequest {
                version: "1.0".to_string(),
                request_id: "subnet-2".to_string(),
                domain: "global".to_string(),
                address: "10.0.1.1".to_string(), // different subnet
                identity: Bytes::new(),
                mechanism: ZapMechanism::Null,
                credentials: vec![],
            };
            let resp2 = handler.authenticate(&allowed).await;
            assert_eq!(
                resp2.status_code,
                ZapStatus::Success,
                "addresses not matching a denied prefix must be accepted"
            );
        });
    }
}
