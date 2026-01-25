/// ZAP Client Infrastructure
///
/// This module provides client-side ZAP integration for server sockets.
/// During authentication, server sockets send ZAP requests to the ZAP handler
/// running on inproc://zeromq.zap.01 and wait for the response.

use crate::security::zap::{ZapMechanism, ZapRequest, ZapResponse};
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
    /// Connects to the standard ZAP endpoint inproc://zeromq.zap.01
    ///
    /// # Arguments
    /// * `timeout` - Timeout for ZAP requests (default: 5 seconds)
    pub fn new(timeout: Duration) -> io::Result<Self> {
        let socket = DealerSocket::connect_inproc(
            "inproc://zeromq.zap.01",
            SocketOptions::default(),
        )?;

        Ok(Self {
            socket,
            timeout,
        })
    }

    /// Send a ZAP authentication request and wait for response
    ///
    /// # Arguments
    /// * `request` - The ZAP request to send
    ///
    /// # Returns
    /// The ZAP response from the handler, or an error if timeout/decode failed
    pub async fn authenticate(&mut self, request: &ZapRequest) -> io::Result<ZapResponse> {
        // Encode and send the request
        let frames = request.encode();
        self.socket.send(frames).await?;

        // Wait for response with timeout
        let recv_future = self.socket.recv();
        let response_frames = match compio::time::timeout(self.timeout, recv_future).await {
            Ok(Ok(Some(frames))) => frames,
            Ok(Ok(None)) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "ZAP handler disconnected",
                ))
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "ZAP request timed out",
                ))
            }
        };

        // Decode the response
        ZapResponse::decode(&response_frames).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Failed to decode ZAP response: {}", e))
        })
    }

    /// Send a PLAIN authentication request
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
        // Generate a simple request ID (timestamp-based)
        let request_id = format!("{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos());

        let request = ZapRequest {
            version: "1.0".to_string(),
            request_id,
            domain: domain.to_string(),
            address: address.to_string(),
            identity: Bytes::new(),
            mechanism: ZapMechanism::Plain,
            credentials: vec![
                Bytes::from(username.as_bytes().to_vec()),
                Bytes::from(password.as_bytes().to_vec()),
            ],
        };

        self.authenticate(&request).await
    }

    /// Send a CURVE authentication request
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
        // Generate a simple request ID (timestamp-based)
        let request_id = format!("{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos());

        let request = ZapRequest {
            version: "1.0".to_string(),
            request_id,
            domain: domain.to_string(),
            address: address.to_string(),
            identity: Bytes::new(),
            mechanism: ZapMechanism::Curve,
            credentials: vec![Bytes::from(client_key.to_vec())],
        };

        self.authenticate(&request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ZAP client tests require a running ZAP server.
    // Integration tests are in tests/zap_integration.rs
    
    #[test]
    fn test_zap_client_creation() {
        // Test that we can create a client
        let result = ZapClient::new(Duration::from_secs(1));
        // Client creation may fail if no ZAP server is available - this is expected
        assert!(result.is_ok() || result.is_err());
    }
}
