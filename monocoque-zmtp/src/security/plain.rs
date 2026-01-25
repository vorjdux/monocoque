//! PLAIN authentication mechanism (RFC 23)
//!
//! PLAIN provides simple username/password authentication using the ZAP protocol.
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
//! ## Protocol Flow
//!
//! **Client → Server: HELLO**
//! ```text
//! [0] 0x05 "HELLO"
//! [1] username (length-prefixed string)
//! [2] password (length-prefixed string)
//! ```
//!
//! **Server → ZAP Handler: REQUEST**
//! ```text
//! Multipart message with username + password
//! ```
//!
//! **ZAP Handler → Server: RESPONSE**
//! ```text
//! Status code (200 = success, 400 = failure)
//! ```
//!
//! **Server → Client: WELCOME or ERROR**
//! ```text
//! WELCOME (if 200) or ERROR (if not 200)
//! ```

use crate::codec::ZmtpError;
use crate::security::zap::{ZapMechanism, ZapRequest, ZapStatus};
use bytes::{Bytes, BytesMut};
use compio::io::{AsyncRead, AsyncWrite};
use std::time::Duration;
use tracing::{debug, warn};

/// PLAIN command identifiers
const PLAIN_HELLO: &[u8] = b"\x05HELLO";
const PLAIN_WELCOME: &[u8] = b"\x07WELCOME";
const PLAIN_ERROR: &[u8] = b"\x05ERROR";

/// PLAIN client credentials
#[derive(Debug, Clone)]
pub struct PlainCredentials {
    pub username: String,
    pub password: String,
}

impl PlainCredentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}

/// PLAIN authentication handler trait
///
/// Implement this to provide custom credential validation.
/// The default implementation rejects all connections.
#[async_trait::async_trait(?Send)]
pub trait PlainAuthHandler {
    /// Validate username and password
    ///
    /// # Arguments
    /// * `username` - Plaintext username
    /// * `password` - Plaintext password
    /// * `domain` - ZAP security domain
    /// * `address` - Peer address (IP:port)
    ///
    /// # Returns
    /// * `Ok(user_id)` - Authentication successful, returns user ID
    /// * `Err(reason)` - Authentication failed, returns error message
    async fn authenticate(
        &self,
        username: &str,
        password: &str,
        domain: &str,
        address: &str,
    ) -> Result<String, String>;
}

/// Simple credential map handler
///
/// Validates against a static HashMap of username → password.
/// For production use, implement PlainAuthHandler with database lookup.
#[derive(Debug, Clone)]
pub struct StaticPlainHandler {
    credentials: std::collections::HashMap<String, String>,
}

impl StaticPlainHandler {
    pub fn new() -> Self {
        Self {
            credentials: std::collections::HashMap::new(),
        }
    }

    pub fn add_user(&mut self, username: impl Into<String>, password: impl Into<String>) {
        self.credentials.insert(username.into(), password.into());
    }
}

impl Default for StaticPlainHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait(?Send)]
impl PlainAuthHandler for StaticPlainHandler {
    async fn authenticate(
        &self,
        username: &str,
        password: &str,
        _domain: &str,
        _address: &str,
    ) -> Result<String, String> {
        match self.credentials.get(username) {
            Some(expected_password) if expected_password == password => {
                Ok(username.to_string())
            }
            Some(_) => Err("Invalid password".to_string()),
            None => Err("Unknown user".to_string()),
        }
    }
}

/// PLAIN client handshake
///
/// Sends HELLO with username/password, waits for WELCOME or ERROR.
pub async fn plain_client_handshake<S>(
    stream: &mut S,
    credentials: &PlainCredentials,
    timeout: Option<Duration>,
) -> Result<(), ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use compio::buf::BufResult;
    use monocoque_core::timeout::write_all_with_timeout;

    debug!("[PLAIN CLIENT] Starting PLAIN authentication for user: {}", 
        credentials.username);

    // Build HELLO command
    let mut hello = BytesMut::new();
    hello.extend_from_slice(PLAIN_HELLO);
    
    // Username (length-prefixed)
    let username_bytes = credentials.username.as_bytes();
    if username_bytes.len() > 255 {
        return Err(ZmtpError::Protocol);
    }
    hello.extend_from_slice(&[username_bytes.len() as u8]);
    hello.extend_from_slice(username_bytes);
    
    // Password (length-prefixed)
    let password_bytes = credentials.password.as_bytes();
    if password_bytes.len() > 255 {
        return Err(ZmtpError::Protocol);
    }
    hello.extend_from_slice(&[password_bytes.len() as u8]);
    hello.extend_from_slice(password_bytes);

    // Send HELLO
    let buf_result = write_all_with_timeout(stream, hello.freeze().to_vec(), timeout).await?;
    let BufResult(result, _) = buf_result;
    result?;

    // Receive WELCOME or ERROR
    let response = vec![0u8; 64]; // Max command size
    let BufResult(result, response) = stream.read(response).await;
    let n = result?;

    if n >= PLAIN_WELCOME.len() && &response[..PLAIN_WELCOME.len()] == PLAIN_WELCOME {
        debug!("[PLAIN CLIENT] Authentication successful");
        Ok(())
    } else if n >= PLAIN_ERROR.len() && &response[..PLAIN_ERROR.len()] == PLAIN_ERROR {
        warn!("[PLAIN CLIENT] Authentication failed");
        Err(ZmtpError::AuthenticationFailed)
    } else {
        warn!("[PLAIN CLIENT] Invalid PLAIN response");
        Err(ZmtpError::Protocol)
    }
}

/// PLAIN server handshake
///
/// Receives HELLO, validates via ZAP handler, sends WELCOME or ERROR.
pub async fn plain_server_handshake<S, H>(
    stream: &mut S,
    handler: &H,
    domain: &str,
    peer_address: &str,
    timeout: Option<Duration>,
) -> Result<String, ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    H: PlainAuthHandler,
{
    use compio::buf::BufResult;
    use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};

    debug!("[PLAIN SERVER] Waiting for PLAIN HELLO from {}", peer_address);

    // Read command header (6 bytes: \x05HELLO)
    let header = vec![0u8; 6];
    let buf_result = read_exact_with_timeout(stream, header, timeout).await?;
    let BufResult(result, header) = buf_result;
    result?;

    if &header[..] != PLAIN_HELLO {
        warn!("[PLAIN SERVER] Invalid PLAIN command header");
        return Err(ZmtpError::Protocol);
    }

    // Read username length
    let len_buf = vec![0u8; 1];
    let buf_result = read_exact_with_timeout(stream, len_buf, timeout).await?;
    let BufResult(result, len_buf) = buf_result;
    result?;
    let username_len = len_buf[0] as usize;

    // Read username
    let username_buf = vec![0u8; username_len];
    let buf_result = read_exact_with_timeout(stream, username_buf, timeout).await?;
    let BufResult(result, username_buf) = buf_result;
    result?;
    let username = String::from_utf8(username_buf)
        .map_err(|_| ZmtpError::Protocol)?;

    // Read password length
    let len_buf = vec![0u8; 1];
    let buf_result = read_exact_with_timeout(stream, len_buf, timeout).await?;
    let BufResult(result, len_buf) = buf_result;
    result?;
    let password_len = len_buf[0] as usize;

    // Read password
    let password_buf = vec![0u8; password_len];
    let buf_result = read_exact_with_timeout(stream, password_buf, timeout).await?;
    let BufResult(result, password_buf) = buf_result;
    result?;
    let password = String::from_utf8(password_buf)
        .map_err(|_| ZmtpError::Protocol)?;

    debug!("[PLAIN SERVER] Received credentials for user: {}", username);

    // Authenticate via handler
    match handler.authenticate(&username, &password, domain, peer_address).await {
        Ok(user_id) => {
            debug!("[PLAIN SERVER] Authentication successful for user: {}", user_id);
            
            // Send WELCOME
            let buf_result = write_all_with_timeout(
                stream,
                PLAIN_WELCOME.to_vec(),
                timeout
            ).await?;
            let BufResult(result, _) = buf_result;
            result?;
            
            Ok(user_id)
        }
        Err(reason) => {
            warn!("[PLAIN SERVER] Authentication failed: {}", reason);
            
            // Send ERROR
            let buf_result = write_all_with_timeout(
                stream,
                PLAIN_ERROR.to_vec(),
                timeout
            ).await?;
            let BufResult(result, _) = buf_result;
            result?;
            
            Err(ZmtpError::AuthenticationFailed)
        }
    }
}

/// PLAIN server handshake using ZAP protocol
///
/// Receives HELLO, sends ZAP request to inproc://zeromq.zap.01, sends WELCOME or ERROR.
/// This is the recommended approach for production deployments.
pub async fn plain_server_handshake_zap<S>(
    stream: &mut S,
    domain: &str,
    peer_address: &str,
    timeout: Option<Duration>,
) -> Result<String, ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use compio::buf::BufResult;
    use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};
    use crate::security::zap_client::ZapClient;

    debug!("[PLAIN SERVER ZAP] Waiting for PLAIN HELLO from {}", peer_address);

    // Read command header (6 bytes: \x05HELLO)
    let header = vec![0u8; 6];
    let buf_result = read_exact_with_timeout(stream, header, timeout).await?;
    let BufResult(result, header) = buf_result;
    result?;

    if &header[..] != PLAIN_HELLO {
        warn!("[PLAIN SERVER ZAP] Invalid PLAIN command header");
        return Err(ZmtpError::Protocol);
    }

    // Read username length
    let len_buf = vec![0u8; 1];
    let buf_result = read_exact_with_timeout(stream, len_buf, timeout).await?;
    let BufResult(result, len_buf) = buf_result;
    result?;
    let username_len = len_buf[0] as usize;

    // Read username
    let username_buf = vec![0u8; username_len];
    let buf_result = read_exact_with_timeout(stream, username_buf, timeout).await?;
    let BufResult(result, username_buf) = buf_result;
    result?;
    let username = String::from_utf8(username_buf)
        .map_err(|_| ZmtpError::Protocol)?;

    // Read password length
    let len_buf = vec![0u8; 1];
    let buf_result = read_exact_with_timeout(stream, len_buf, timeout).await?;
    let BufResult(result, len_buf) = buf_result;
    result?;
    let password_len = len_buf[0] as usize;

    // Read password
    let password_buf = vec![0u8; password_len];
    let buf_result = read_exact_with_timeout(stream, password_buf, timeout).await?;
    let BufResult(result, password_buf) = buf_result;
    result?;
    let password = String::from_utf8(password_buf)
        .map_err(|_| ZmtpError::Protocol)?;

    debug!("[PLAIN SERVER ZAP] Received credentials for user: {}, sending ZAP request", username);

    // Create ZAP client and send authentication request
    let mut zap_client = ZapClient::new(Duration::from_secs(5))
        .map_err(|_| {
            warn!("[PLAIN SERVER ZAP] Failed to connect to ZAP handler");
            ZmtpError::AuthenticationFailed
        })?;

    let zap_response = zap_client
        .authenticate_plain(&username, &password, domain, peer_address)
        .await
        .map_err(|e| {
            warn!("[PLAIN SERVER ZAP] ZAP request failed: {}", e);
            ZmtpError::AuthenticationFailed
        })?;

    // Check ZAP response status
    if matches!(zap_response.status_code, ZapStatus::Success) {
        debug!("[PLAIN SERVER ZAP] Authentication successful for user: {}", zap_response.user_id);
        
        // Send WELCOME
        let buf_result = write_all_with_timeout(
            stream,
            PLAIN_WELCOME.to_vec(),
            timeout
        ).await?;
        let BufResult(result, _) = buf_result;
        result?;
        
        Ok(zap_response.user_id)
    } else {
        warn!("[PLAIN SERVER ZAP] Authentication failed: {}", zap_response.status_text);
        
        // Send ERROR
        let buf_result = write_all_with_timeout(
            stream,
            PLAIN_ERROR.to_vec(),
            timeout
        ).await?;
        let BufResult(result, _) = buf_result;
        result?;
        
        Err(ZmtpError::AuthenticationFailed)
    }
}

/// Create a ZAP request for PLAIN authentication
pub fn create_plain_zap_request(
    request_id: impl Into<String>,
    domain: impl Into<String>,
    address: impl Into<String>,
    identity: Bytes,
    username: impl Into<String>,
    password: impl Into<String>,
) -> ZapRequest {
    ZapRequest::new(
        request_id,
        domain,
        address,
        identity,
        ZapMechanism::Plain,
        vec![
            Bytes::from(username.into()),
            Bytes::from(password.into()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[compio::test]
    async fn test_static_plain_handler() {
        let mut handler = StaticPlainHandler::new();
        handler.add_user("admin", "secret123");
        handler.add_user("guest", "guest123");

        // Valid credentials
        let result = handler.authenticate("admin", "secret123", "test", "127.0.0.1").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "admin");

        // Invalid password
        let result = handler.authenticate("admin", "wrong", "test", "127.0.0.1").await;
        assert!(result.is_err());

        // Unknown user
        let result = handler.authenticate("unknown", "password", "test", "127.0.0.1").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_plain_zap_request() {
        let request = create_plain_zap_request(
            "req123",
            "production",
            "192.168.1.100:5555",
            Bytes::from("client1"),
            "testuser",
            "testpass",
        );

        assert_eq!(request.mechanism, ZapMechanism::Plain);
        assert_eq!(request.credentials.len(), 2);
        assert_eq!(&request.credentials[0][..], b"testuser");
        assert_eq!(&request.credentials[1][..], b"testpass");
    }
}
