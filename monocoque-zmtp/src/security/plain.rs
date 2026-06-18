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
use crate::security::protocol::reject_immediately_available_trailing_bytes;
use crate::security::zap::{ZapMechanism, ZapRequest, ZapStatus};
use bytes::{Bytes, BytesMut};
use compio_io::{AsyncRead, AsyncWrite};
use std::fmt;
use std::time::Duration;
use tracing::{debug, warn};

/// PLAIN command identifiers
const PLAIN_HELLO: &[u8] = b"\x05HELLO";
const PLAIN_WELCOME: &[u8] = b"\x07WELCOME";
const PLAIN_ERROR: &[u8] = b"\x05ERROR";
const TRAILING_BYTE_CHECK_TIMEOUT: Duration = Duration::from_millis(1);

/// PLAIN client credentials
#[derive(Clone)]
pub struct PlainCredentials {
    /// Plaintext username.
    pub username: String,
    /// Plaintext password.
    pub password: String,
}

impl fmt::Debug for PlainCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlainCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl PlainCredentials {
    /// Create new credentials from the given username and password.
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
#[derive(Clone)]
pub struct StaticPlainHandler {
    credentials: std::collections::HashMap<String, String>,
}

impl fmt::Debug for StaticPlainHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticPlainHandler")
            .field("credential_count", &self.credentials.len())
            .finish()
    }
}

impl StaticPlainHandler {
    /// Create a new handler with an empty credential map.
    pub fn new() -> Self {
        Self {
            credentials: std::collections::HashMap::new(),
        }
    }

    /// Register a username/password pair in the credential map.
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
        use subtle::ConstantTimeEq;
        match self.credentials.get(username) {
            Some(expected_password)
                if expected_password
                    .as_bytes()
                    .ct_eq(password.as_bytes())
                    .into() =>
            {
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
    use compio_buf::BufResult;
    use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};

    debug!(
        "[PLAIN CLIENT] Starting PLAIN authentication for user: {}",
        credentials.username
    );

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

    // Read the first byte to determine response type (command name length)
    let len_buf = vec![0u8; 1];
    let BufResult(res, len_buf) = read_exact_with_timeout(stream, len_buf, timeout).await?;
    res?;
    let cmd_len = len_buf[0] as usize;
    if cmd_len == 0 || cmd_len > 32 {
        warn!(
            "[PLAIN CLIENT] Invalid PLAIN response command length: {}",
            cmd_len
        );
        return Err(ZmtpError::Protocol);
    }
    // Read command name
    let cmd_buf = vec![0u8; cmd_len];
    let BufResult(res, cmd_buf) = read_exact_with_timeout(stream, cmd_buf, timeout).await?;
    res?;

    match cmd_buf.as_slice() {
        b"WELCOME" => {
            debug!("[PLAIN CLIENT] Authentication successful");
            Ok(())
        }
        b"ERROR" => {
            warn!("[PLAIN CLIENT] Authentication failed");
            Err(ZmtpError::AuthenticationFailed)
        }
        other => {
            warn!(
                "[PLAIN CLIENT] Invalid PLAIN response command: {:?}",
                String::from_utf8_lossy(other)
            );
            Err(ZmtpError::Protocol)
        }
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
    use compio_buf::BufResult;
    use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};

    debug!(
        "[PLAIN SERVER] Waiting for PLAIN HELLO from {}",
        peer_address
    );

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
    let username = String::from_utf8(username_buf).map_err(|_| ZmtpError::Protocol)?;

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
    let password = String::from_utf8(password_buf).map_err(|_| ZmtpError::Protocol)?;
    reject_immediately_available_trailing_bytes(stream, TRAILING_BYTE_CHECK_TIMEOUT).await?;

    debug!("[PLAIN SERVER] Received credentials for user: {}", username);

    // Authenticate via handler
    match handler
        .authenticate(&username, &password, domain, peer_address)
        .await
    {
        Ok(user_id) => {
            debug!(
                "[PLAIN SERVER] Authentication successful for user: {}",
                user_id
            );

            // Send WELCOME
            let buf_result =
                write_all_with_timeout(stream, PLAIN_WELCOME.to_vec(), timeout).await?;
            let BufResult(result, _) = buf_result;
            result?;

            Ok(user_id)
        }
        Err(reason) => {
            warn!("[PLAIN SERVER] Authentication failed: {}", reason);

            // Send ERROR
            let buf_result = write_all_with_timeout(stream, PLAIN_ERROR.to_vec(), timeout).await?;
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
    use crate::security::zap_client::ZapClient;
    use compio_buf::BufResult;
    use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};

    debug!(
        "[PLAIN SERVER ZAP] Waiting for PLAIN HELLO from {}",
        peer_address
    );

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
    let username = String::from_utf8(username_buf).map_err(|_| ZmtpError::Protocol)?;

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
    let password = String::from_utf8(password_buf).map_err(|_| ZmtpError::Protocol)?;
    reject_immediately_available_trailing_bytes(stream, TRAILING_BYTE_CHECK_TIMEOUT).await?;

    debug!(
        "[PLAIN SERVER ZAP] Received credentials for user: {}, sending ZAP request",
        username
    );

    // Create ZAP client and send authentication request
    let mut zap_client = ZapClient::new(Duration::from_secs(5)).map_err(|_| {
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
        debug!(
            "[PLAIN SERVER ZAP] Authentication successful for user: {}",
            zap_response.user_id
        );

        // Send WELCOME
        let buf_result = write_all_with_timeout(stream, PLAIN_WELCOME.to_vec(), timeout).await?;
        let BufResult(result, _) = buf_result;
        result?;

        Ok(zap_response.user_id)
    } else {
        warn!(
            "[PLAIN SERVER ZAP] Authentication failed: {}",
            zap_response.status_text
        );

        // Send ERROR
        let buf_result = write_all_with_timeout(stream, PLAIN_ERROR.to_vec(), timeout).await?;
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
        vec![Bytes::from(username.into()), Bytes::from(password.into())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_static_plain_handler() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_static_plain_handler_impl())
    }

    async fn test_static_plain_handler_impl() {
        let mut handler = StaticPlainHandler::new();
        handler.add_user("admin", "secret123");
        handler.add_user("guest", "guest123");

        // Valid credentials
        let result = handler
            .authenticate("admin", "secret123", "test", "127.0.0.1")
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "admin");

        // Invalid password
        let result = handler
            .authenticate("admin", "wrong", "test", "127.0.0.1")
            .await;
        assert!(result.is_err());

        // Unknown user
        let result = handler
            .authenticate("unknown", "password", "test", "127.0.0.1")
            .await;
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

    #[compio::test]
    async fn plain_server_rejects_hello_with_trailing_credential_bytes() {
        use compio::buf::BufResult;
        use compio::net::{TcpListener, TcpStream};
        use compio::runtime;
        use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = runtime::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut handler = StaticPlainHandler::new();
            handler.add_user("admin", "secret");

            plain_server_handshake(
                &mut stream,
                &handler,
                "global",
                "127.0.0.1:1",
                Some(Duration::from_secs(1)),
            )
            .await
        });

        let mut stream = TcpStream::connect(addr).await.unwrap();
        let mut hello = plain_hello(b"admin", b"secret");
        hello.extend_from_slice(b"\x05extra");
        let BufResult(write_result, _) =
            write_all_with_timeout(&mut stream, hello, Some(Duration::from_secs(1)))
                .await
                .unwrap();
        write_result.unwrap();

        let response = vec![0u8; PLAIN_WELCOME.len()];
        let BufResult(read_result, response) =
            read_exact_with_timeout(&mut stream, response, Some(Duration::from_secs(1)))
                .await
                .unwrap();
        let _ = read_result;

        let result = server_task.await;
        assert!(
            result.is_err() && response.as_slice() != PLAIN_WELCOME,
            "PLAIN server authenticated a HELLO command with trailing credential bytes"
        );
    }

    #[test]
    fn debug_output_redacts_static_plain_handler_passwords() {
        let mut handler = StaticPlainHandler::new();
        handler.add_user("alice", "handler-password");

        let debug = format!("{handler:?}");

        assert!(
            !debug.contains("handler-password"),
            "StaticPlainHandler Debug output exposes stored PLAIN passwords"
        );
    }
}
