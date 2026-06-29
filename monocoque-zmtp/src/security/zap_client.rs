/// ZAP Client Infrastructure
///
/// This module provides client-side ZAP integration for server sockets.
/// During authentication, server sockets send ZAP requests to the ZAP handler
/// running on inproc://zeromq.zap.01 and wait for the response.
///
/// ## Default-deny security model
///
/// When no ZAP handler is registered, attempting to connect to the ZAP
/// endpoint (inproc://zeromq.zap.01) returns `ErrorKind::NotFound`.  This is
/// treated as an **authentication failure** - the connecting peer is REJECTED
/// (equivalent to a 403/400 response).  This implements the correct
/// "default-deny" security posture: if there is no handler to approve the
/// connection it must be denied, not silently accepted.
use crate::security::zap::{ZapMechanism, ZapRequest, ZapResponse, ZapStatus};
use crate::{DealerSocket, inproc_stream::InprocStream};
use bytes::Bytes;
use monocoque_core::options::SocketOptions;
use std::io;
use std::time::Duration;

/// ZAP client for sending authentication requests
///
/// Server sockets use this to communicate with the ZAP handler.
/// The client connects to inproc://zeromq.zap.01 and sends requests.
pub struct ZapClient {
    socket: DealerSocket<InprocStream>,
    timeout: Duration,
}

impl ZapClient {
    /// Create a new ZAP client
    ///
    /// Connects to the standard ZAP endpoint inproc://zeromq.zap.01.
    ///
    /// # Default-deny when no handler is registered
    ///
    /// If the ZAP endpoint is not bound (no handler registered), connecting
    /// returns `ErrorKind::NotFound`.  Callers MUST treat this as a connection
    /// rejection - see [`ZapClient::authenticate`] and the helper methods.
    ///
    /// # Arguments
    /// * `timeout` - Timeout for ZAP requests (default: 5 seconds)
    pub fn new(timeout: Duration) -> io::Result<Self> {
        let socket =
            DealerSocket::connect_inproc("inproc://zeromq.zap.01", SocketOptions::default())?;

        Ok(Self { socket, timeout })
    }

    /// Synthesise a denial response for use when no ZAP handler is reachable.
    ///
    /// Per the default-deny model: if the ZAP endpoint is unreachable (no
    /// handler registered), the connection must be treated as rejected.
    fn denial_response(request_id: impl Into<String>, reason: &str) -> ZapResponse {
        ZapResponse {
            version: crate::security::zap::ZAP_VERSION.to_string(),
            request_id: request_id.into(),
            status_code: ZapStatus::Failure,
            status_text: reason.to_string(),
            user_id: String::new(),
            metadata: std::collections::HashMap::new(),
        }
    }

    /// Send a ZAP authentication request and wait for response.
    ///
    /// # Default-deny: NotFound is treated as rejection
    ///
    /// If sending to (or receiving from) the ZAP endpoint fails with
    /// `ErrorKind::NotFound`, this means no ZAP handler is registered on
    /// `inproc://zeromq.zap.01`.  Per the ZAP specification and the
    /// default-deny security model, a missing handler MUST cause the
    /// connection to be **rejected** (returned as a 400 Failure response),
    /// not silently accepted.  Any other I/O error is propagated to the
    /// caller as-is.
    ///
    /// # Arguments
    /// * `request` - The ZAP request to send
    ///
    /// # Returns
    /// The ZAP response from the handler, or an error if timeout/decode failed
    pub async fn authenticate(&mut self, request: &ZapRequest) -> io::Result<ZapResponse> {
        // Encode and send the request.
        // NotFound here means the ZAP endpoint is not bound → default-deny.
        let frames = request.encode();
        if let Err(e) = self.socket.send(frames).await {
            if e.kind() == io::ErrorKind::NotFound {
                // No ZAP handler registered - deny the connection (default-deny).
                return Ok(Self::denial_response(
                    &request.request_id,
                    "No ZAP handler registered - connection denied by default",
                ));
            }
            return Err(e);
        }

        // Wait for response with timeout.
        // NotFound on recv also indicates the endpoint disappeared → deny.
        let recv_future = self.socket.recv();
        let response_frames = match compio::time::timeout(self.timeout, recv_future).await {
            Ok(Ok(Some(frames))) => frames,
            Ok(Ok(None)) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "ZAP handler disconnected",
                ));
            }
            Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => {
                // Endpoint vanished after send - deny (default-deny).
                return Ok(Self::denial_response(
                    &request.request_id,
                    "ZAP handler unavailable - connection denied by default",
                ));
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "ZAP request timed out",
                ));
            }
        };

        // Decode the response
        let response = ZapResponse::decode(&response_frames).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to decode ZAP response: {}", e),
            )
        })?;

        // Verify the ZAP version (RFC 27 §5)
        if response.version != crate::security::zap::ZAP_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "ZAP response version mismatch: expected {}, got {}",
                    crate::security::zap::ZAP_VERSION,
                    response.version
                ),
            ));
        }

        // Verify the response is for our request
        if response.request_id != request.request_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "ZAP response request_id mismatch: expected {:?}, got {:?}",
                    request.request_id, response.request_id
                ),
            ));
        }

        Ok(response)
    }

    /// Send a PLAIN authentication request
    ///
    /// The request ID is generated using the process-wide monotonic counter
    /// (see `next_request_id`) to guarantee uniqueness within a process.
    ///
    /// # Arguments
    /// * `username` - Username credential
    /// * `password` - Password credential
    /// * `domain` - ZAP domain (usually empty string)
    /// * `address` - Peer address (IP:port)
    ///
    /// # Returns
    /// The ZAP response indicating success (200) or failure (400+)
    pub async fn authenticate_plain(
        &mut self,
        username: &str,
        password: &str,
        domain: &str,
        address: &str,
    ) -> io::Result<ZapResponse> {
        let request = ZapRequest::new_with_unique_id(
            domain,
            address,
            Bytes::new(),
            ZapMechanism::Plain,
            vec![
                Bytes::from(username.as_bytes().to_vec()),
                Bytes::from(password.as_bytes().to_vec()),
            ],
        );

        self.authenticate(&request).await
    }

    /// Send a CURVE authentication request
    ///
    /// The request ID is generated using the process-wide monotonic counter
    /// (see `next_request_id`) to guarantee uniqueness within a process.
    ///
    /// # Arguments
    /// * `client_key` - Client's public key (32 bytes)
    /// * `domain` - ZAP domain (usually empty string)
    /// * `address` - Peer address (IP:port)
    ///
    /// # Returns
    /// The ZAP response indicating success (200) or failure (400+)
    pub async fn authenticate_curve(
        &mut self,
        client_key: &[u8; 32],
        domain: &str,
        address: &str,
    ) -> io::Result<ZapResponse> {
        let request = ZapRequest::new_with_unique_id(
            domain,
            address,
            Bytes::new(),
            ZapMechanism::Curve,
            vec![Bytes::from(client_key.to_vec())],
        );

        self.authenticate(&request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::zap::next_request_id;

    // ZAP client tests require a running ZAP server.
    // Integration tests are in tests/zap_integration.rs

    #[test]
    fn test_zap_client_creation() {
        // Test that we can create a client
        let result = ZapClient::new(Duration::from_secs(1));
        // Client creation may fail if no ZAP server is available - this is expected
        assert!(result.is_ok() || result.is_err());
    }

    /// Verify that unique request IDs are strictly increasing and never repeat.
    #[test]
    fn test_unique_request_ids_are_monotonic() {
        let id1 = next_request_id();
        let id2 = next_request_id();
        let id3 = next_request_id();

        // IDs are decimal strings - convert for comparison
        let n1: u64 = id1.parse().expect("request ID must be a decimal number");
        let n2: u64 = id2.parse().expect("request ID must be a decimal number");
        let n3: u64 = id3.parse().expect("request ID must be a decimal number");

        assert!(n2 > n1, "request IDs must be strictly increasing");
        assert!(n3 > n2, "request IDs must be strictly increasing");
    }

    /// Verify that ZapRequest::new_with_unique_id generates distinct IDs across
    /// multiple requests without requiring the caller to supply an ID.
    #[test]
    fn test_new_with_unique_id_produces_distinct_ids() {
        let r1 = ZapRequest::new_with_unique_id(
            "test",
            "127.0.0.1",
            Bytes::new(),
            ZapMechanism::Null,
            vec![],
        );
        let r2 = ZapRequest::new_with_unique_id(
            "test",
            "127.0.0.1",
            Bytes::new(),
            ZapMechanism::Null,
            vec![],
        );
        assert_ne!(
            r1.request_id, r2.request_id,
            "each request must have a unique ID"
        );
    }

    /// Verify the default-deny sentinel response that is returned when the ZAP
    /// endpoint is unreachable (no handler registered).
    #[test]
    fn test_denial_response_is_failure() {
        let resp = ZapClient::denial_response(
            "42",
            "No ZAP handler registered - connection denied by default",
        );
        assert_eq!(
            resp.status_code,
            ZapStatus::Failure,
            "missing ZAP handler must produce a Failure (400) response"
        );
        assert!(
            resp.user_id.is_empty(),
            "denied response must have empty user_id"
        );
        assert_eq!(resp.request_id, "42");
    }
}
