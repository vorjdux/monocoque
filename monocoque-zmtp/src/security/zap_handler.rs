/// ZAP (ZeroMQ Authentication Protocol) Handler Infrastructure
///
/// This module provides the ZAP handler infrastructure for authentication.
/// ZAP handlers run on inproc://zeromq.zap.01 and process authentication
/// requests from server sockets.

use crate::security::plain::PlainAuthHandler;
use crate::security::zap::{ZapMechanism, ZapRequest, ZapResponse};
use crate::{DealerSocket, inproc_stream::InprocStream};
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
}

impl<H: PlainAuthHandler> DefaultZapHandler<H> {
    /// Create a new default ZAP handler
    ///
    /// # Arguments
    /// * `plain_handler` - Handler for PLAIN authentication
    /// * `accept_curve` - Whether to accept CURVE connections (default: true)
    pub fn new(plain_handler: Arc<H>, accept_curve: bool) -> Self {
        Self {
            plain_handler,
            accept_curve,
        }
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
                    return ZapResponse::failure(
                        request.request_id.clone(),
                        "Missing credentials",
                    );
                }

                let username = String::from_utf8_lossy(&request.credentials[0]);
                let password = String::from_utf8_lossy(&request.credentials[1]);

                // Call PLAIN handler
                match self
                    .plain_handler
                    .authenticate(
                        &username,
                        &password,
                        &request.domain,
                        &request.address,
                    )
                    .await
                {
                    Ok(user_id) => ZapResponse::success(request.request_id.clone(), user_id),
                    Err(err) => ZapResponse::failure(request.request_id.clone(), &err),
                }
            }
            ZapMechanism::Curve => {
                // CURVE mechanism - verify public key is present
                if request.credentials.is_empty() {
                    return ZapResponse::failure(
                        request.request_id.clone(),
                        "Missing CURVE public key",
                    );
                }

                if !self.accept_curve {
                    return ZapResponse::failure(
                        request.request_id.clone(),
                        "CURVE not enabled",
                    );
                }

                // In a real implementation, you would check the public key
                // against a whitelist/blacklist. For now, accept all valid CURVE keys.
                let public_key = &request.credentials[0];
                if public_key.len() != 32 {
                    return ZapResponse::failure(
                        request.request_id.clone(),
                        "Invalid CURVE key length",
                    );
                }

                // Use the hex-encoded public key as user_id
                let user_id = format!("{:x?}", public_key);
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
    /// use std::collections::HashMap;
    /// use std::sync::Arc;
    ///
    /// fn run_zap_server() -> std::io::Result<()> {
    ///     // Create a simple PLAIN handler
    ///     let mut credentials = HashMap::new();
    ///     credentials.insert("admin".to_string(), "secret".to_string());
    ///     let plain_handler = Arc::new(StaticPlainHandler::new(credentials));
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
        let socket = DealerSocket::bind_inproc(
            "inproc://zeromq.zap.01",
            SocketOptions::default(),
        )?;

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
            let msg = match self.socket.recv().await? {
                Some(frames) => frames,
                None => continue,
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
/// use std::collections::HashMap;
/// use std::sync::Arc;
///
/// fn setup_auth() -> std::io::Result<()> {
///     let mut credentials = HashMap::new();
///     credentials.insert("admin".to_string(), "secret".to_string());
///     let plain_handler = Arc::new(StaticPlainHandler::new(credentials));
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
/// use std::collections::HashMap;
/// use std::sync::Arc;
///
/// fn setup() -> std::io::Result<()> {
///     let mut credentials = HashMap::new();
///     credentials.insert("admin".to_string(), "secret".to_string());
///     let handler = Arc::new(StaticPlainHandler::new(credentials));
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
    use std::collections::HashMap;

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
}
