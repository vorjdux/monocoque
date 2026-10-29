/// ZAP Client Infrastructure
///
/// This module provides client-side ZAP integration for server sockets.
/// During authentication, server sockets send ZAP requests to the ZAP handler
/// running on inproc://zeromq.zap.01 and wait for the response.
///
/// ## Default-deny security model
///
/// When no ZAP handler is registered, connecting to the ZAP endpoint
/// (inproc://zeromq.zap.01) fails with `ErrorKind::ConnectionRefused`. This is
/// treated as an **authentication failure** - the connecting peer is REJECTED
/// (equivalent to a 403/400 response).  This implements the correct
/// "default-deny" security posture: if there is no handler to approve the
/// connection it must be denied, not silently accepted.
use crate::security::zap::{ZapMechanism, ZapRequest, ZapResponse};
use crate::sock::kinds::DealerSocket;
use bytes::Bytes;
use monocoque_core::msg::Msg;
use std::io;
use std::time::Duration;

/// The standard ZAP endpoint.
pub const ZAP_ENDPOINT: &str = "inproc://zeromq.zap.01";

/// ZAP client for sending authentication requests
///
/// Server sockets use this to communicate with the ZAP handler.
/// The client is a DEALER connected to inproc://zeromq.zap.01.
pub struct ZapClient {
    socket: DealerSocket,
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
    /// fails with `ErrorKind::ConnectionRefused`. Callers MUST treat this as a
    /// connection rejection.
    ///
    /// # Arguments
    /// * `timeout` - Timeout for ZAP requests (default: 5 seconds)
    pub async fn new(timeout: Duration) -> io::Result<Self> {
        let mut socket = DealerSocket::new();
        socket.connect(ZAP_ENDPOINT).await?;
        Ok(Self { socket, timeout })
    }

    /// Send a ZAP authentication request and wait for response.
    ///
    /// # Default-deny
    ///
    /// The handler going away before it answers is treated as a rejection:
    /// the request either fails or times out, and the caller denies the
    /// connection. A missing handler MUST never let a peer through.
    ///
    /// # Arguments
    /// * `request` - The ZAP request to send
    ///
    /// # Returns
    /// The ZAP response from the handler, or an error if timeout/decode failed
    pub async fn authenticate(&mut self, request: &ZapRequest) -> io::Result<ZapResponse> {
        let msg: Msg = request.encode().into_iter().collect();
        let exchange = async {
            self.socket.send(msg).await?;
            self.socket.recv_owned().await
        };
        let response_frames = match monocoque_core::rt::timeout(self.timeout, exchange).await {
            Ok(Ok(msg)) => msg.into_frames(),
            Ok(Err(e)) => return Err(e.into()),
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
    use crate::security::zap::{ZapStatus, next_request_id};

    // ZAP client tests require a running ZAP server.
    // Integration tests are in tests/zap_integration.rs

    /// No handler bound means the client cannot connect (default-deny); once a
    /// handler is bound, a request gets its matching response. Both halves
    /// share one test because the ZAP endpoint is process-global.
    #[test]
    fn test_zap_client_refused_then_answered() {
        use crate::security::plain::StaticPlainHandler;
        use crate::security::zap_handler::start_default_zap_server;
        use std::sync::Arc;

        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async {
                let err = ZapClient::new(Duration::from_secs(1))
                    .await
                    .err()
                    .expect("no ZAP handler is bound");
                assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);

                let mut users = StaticPlainHandler::new();
                users.add_user("admin", "secret");
                start_default_zap_server(Arc::new(users), false).unwrap();

                let mut client = ZapClient::new(Duration::from_secs(5)).await.unwrap();
                let ok = client
                    .authenticate_plain("admin", "secret", "", "127.0.0.1")
                    .await
                    .unwrap();
                assert_eq!(ok.status_code, ZapStatus::Success);
                assert_eq!(ok.user_id, "admin");
                let denied = client
                    .authenticate_plain("admin", "wrong", "", "127.0.0.1")
                    .await
                    .unwrap();
                assert_eq!(denied.status_code, ZapStatus::Failure);
            });
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

}
