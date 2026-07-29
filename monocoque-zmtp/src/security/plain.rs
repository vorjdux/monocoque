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
const TRAILING_BYTE_CHECK_TIMEOUT: Duration = Duration::from_millis(10);
/// Upper bound on a PLAIN command body (HELLO/WELCOME/ERROR are tiny; two
/// length-prefixed credentials are at most ~512 bytes).
const MAX_PLAIN_CMD_BODY: usize = 512;

/// Parse a PLAIN HELLO command body: `\x05HELLO <ulen><username> <plen><password>`.
fn parse_plain_hello(body: &[u8]) -> Result<(String, String), ZmtpError> {
    if !body.starts_with(PLAIN_HELLO) {
        return Err(ZmtpError::Protocol);
    }
    let mut off = PLAIN_HELLO.len();

    let ulen = *body.get(off).ok_or(ZmtpError::Protocol)? as usize;
    off += 1;
    let uend = off
        .checked_add(ulen)
        .filter(|&e| e <= body.len())
        .ok_or(ZmtpError::Protocol)?;
    let username = String::from_utf8(body[off..uend].to_vec()).map_err(|_| ZmtpError::Protocol)?;
    off = uend;

    let plen = *body.get(off).ok_or(ZmtpError::Protocol)? as usize;
    off += 1;
    let pend = off
        .checked_add(plen)
        .filter(|&e| e <= body.len())
        .ok_or(ZmtpError::Protocol)?;
    let password = String::from_utf8(body[off..pend].to_vec()).map_err(|_| ZmtpError::Protocol)?;

    Ok((username, password))
}

/// Read a framed PLAIN HELLO command and return the parsed credentials.
async fn read_plain_hello<S>(
    stream: &mut S,
    timeout: Option<Duration>,
) -> Result<(String, String), ZmtpError>
where
    S: AsyncRead + Unpin,
{
    let body = crate::security::curve::read_zmtp_cmd(stream, timeout, MAX_PLAIN_CMD_BODY).await?;
    parse_plain_hello(&body)
}

/// Write a PLAIN command body wrapped in a ZMTP command frame.
async fn write_plain_cmd<S>(
    stream: &mut S,
    body: &[u8],
    timeout: Option<Duration>,
) -> Result<(), ZmtpError>
where
    S: AsyncWrite + Unpin,
{
    use compio_buf::BufResult;
    use monocoque_core::timeout::write_all_with_timeout;
    let mut framed = BytesMut::new();
    crate::base::append_zmtp_cmd_frame(&mut framed, body);
    let BufResult(result, _) = write_all_with_timeout(stream, framed.freeze().to_vec(), timeout).await?;
    result?;
    Ok(())
}

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
    /// Passwords are wrapped in `Zeroizing` so the plaintext is scrubbed from
    /// memory when an entry (or the whole map) is dropped, rather than lingering
    /// in freed heap.
    credentials: std::collections::HashMap<String, zeroize::Zeroizing<String>>,
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
        self.credentials
            .insert(username.into(), zeroize::Zeroizing::new(password.into()));
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
        use sha2::{Digest, Sha256};
        use subtle::ConstantTimeEq;

        // Always run a constant-time comparison, even when the username is
        // unknown, so response timing cannot distinguish "unknown user" from
        // "known user, wrong password" (username enumeration). On a miss we
        // compare the supplied password against a fixed dummy value instead of
        // returning early.
        const DUMMY_PASSWORD: &str = "\0monocoque-plain-miss-placeholder\0";
        let expected = self.credentials.get(username);
        let reference = expected.map_or(DUMMY_PASSWORD, |p| p.as_str());
        // Compare fixed-width digests, not the passwords directly: subtle's
        // slice ct_eq short-circuits when the lengths differ, which leaks the
        // stored password's length. Hashing both to 32 bytes makes the compared
        // width constant regardless of either password's length.
        let reference_digest = Sha256::digest(reference.as_bytes());
        let supplied_digest = Sha256::digest(password.as_bytes());
        let password_matches: bool = reference_digest
            .as_slice()
            .ct_eq(supplied_digest.as_slice())
            .into();

        if expected.is_some() && password_matches {
            Ok(username.to_string())
        } else {
            // Single, indistinguishable error for both bad-password and
            // unknown-user so the reason string does not leak which failed.
            Err("Invalid credentials".to_string())
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
    use monocoque_core::timeout::write_all_with_timeout;

    debug!("[PLAIN CLIENT] Starting PLAIN authentication");

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

    // Send HELLO wrapped in a ZMTP command frame ([0x04][len][body]) so a real
    // libzmq peer can parse it. (Previously the raw command body was written
    // with no frame header, which only interoperated with monocoque itself.)
    let mut framed = BytesMut::new();
    crate::base::append_zmtp_cmd_frame(&mut framed, &hello);
    let buf_result = write_all_with_timeout(stream, framed.freeze().to_vec(), timeout).await?;
    let BufResult(result, _) = buf_result;
    result?;

    // Read the response as a framed ZMTP command and match on its body.
    let body =
        crate::security::curve::read_zmtp_cmd(stream, timeout, MAX_PLAIN_CMD_BODY).await?;
    if body.starts_with(PLAIN_WELCOME) {
        debug!("[PLAIN CLIENT] Authentication successful");
        Ok(())
    } else if body.starts_with(PLAIN_ERROR) {
        warn!("[PLAIN CLIENT] Authentication failed");
        Err(ZmtpError::AuthenticationFailed)
    } else {
        warn!(
            "[PLAIN CLIENT] Invalid PLAIN response command: {:?}",
            String::from_utf8_lossy(&body)
        );
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
    debug!(
        "[PLAIN SERVER] Waiting for PLAIN HELLO from {}",
        peer_address
    );

    // Read the framed HELLO command and parse the credentials.
    let (username, password) = read_plain_hello(stream, timeout).await?;
    reject_immediately_available_trailing_bytes(stream, TRAILING_BYTE_CHECK_TIMEOUT).await?;

    debug!("[PLAIN SERVER] Received credentials");

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

            write_plain_cmd(stream, PLAIN_WELCOME, timeout).await?;
            Ok(user_id)
        }
        Err(reason) => {
            warn!("[PLAIN SERVER] Authentication failed: {}", reason);
            write_plain_cmd(stream, PLAIN_ERROR, timeout).await?;
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

    debug!(
        "[PLAIN SERVER ZAP] Waiting for PLAIN HELLO from {}",
        peer_address
    );

    // Read the framed HELLO command and parse the credentials.
    let (username, password) = read_plain_hello(stream, timeout).await?;
    reject_immediately_available_trailing_bytes(stream, TRAILING_BYTE_CHECK_TIMEOUT).await?;

    debug!("[PLAIN SERVER ZAP] Received credentials, sending ZAP request");

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

        write_plain_cmd(stream, PLAIN_WELCOME, timeout).await?;
        Ok(zap_response.user_id)
    } else {
        warn!(
            "[PLAIN SERVER ZAP] Authentication failed: {}",
            zap_response.status_text
        );
        write_plain_cmd(stream, PLAIN_ERROR, timeout).await?;
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

    #[cfg(feature = "runtime-compio")]
    fn plain_hello(username: &[u8], password: &[u8]) -> Vec<u8> {
        let mut hello = Vec::new();
        hello.extend_from_slice(PLAIN_HELLO);
        hello.push(username.len() as u8);
        hello.extend_from_slice(username);
        hello.push(password.len() as u8);
        hello.extend_from_slice(password);
        hello
    }

    #[test]
    fn test_static_plain_handler() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_static_plain_handler_impl());
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
        let wrong_password = handler
            .authenticate("admin", "wrong", "test", "127.0.0.1")
            .await;
        assert!(wrong_password.is_err());

        // Unknown user
        let unknown_user = handler
            .authenticate("unknown", "password", "test", "127.0.0.1")
            .await;
        assert!(unknown_user.is_err());

        // The two failure modes must be indistinguishable in their error
        // reason, so a caller cannot enumerate valid usernames from the
        // response. (The constant-time compare closes the timing channel.)
        assert_eq!(
            wrong_password.unwrap_err(),
            unknown_user.unwrap_err(),
            "wrong-password and unknown-user must return the same error"
        );
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

    #[cfg(feature = "runtime-compio")]
    #[test]
    fn plain_server_rejects_hello_with_trailing_credential_bytes() {
        use compio_buf::BufResult;
        use monocoque_core::rt::{LocalRuntime, TcpListener, TcpStream};
        use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};
        use std::time::Duration;

        LocalRuntime::new().unwrap().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server_task = monocoque_core::rt::spawn(async move {
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
            // Frame the HELLO as a ZMTP command, then append stray trailing bytes
            // on the wire that the server must reject after reading the command.
            let body = plain_hello(b"admin", b"secret");
            let mut framed = Vec::new();
            framed.push(0x04);
            framed.push(body.len() as u8);
            framed.extend_from_slice(&body);
            framed.extend_from_slice(b"\x05extra");
            let BufResult(write_result, _) =
                write_all_with_timeout(&mut stream, framed, Some(Duration::from_secs(1)))
                    .await
                    .unwrap();
            write_result.unwrap();

            // The server rejects and closes, so the WELCOME read may EOF; either
            // way it must not return a WELCOME.
            let response = vec![0u8; PLAIN_WELCOME.len()];
            let read = read_exact_with_timeout(&mut stream, response, Some(Duration::from_secs(1)))
                .await;
            let got_welcome = matches!(
                &read,
                Ok(BufResult(Ok(()), resp)) if resp.as_slice() == PLAIN_WELCOME
            );

            let result = monocoque_core::rt::join(server_task).await;
            assert!(
                result.is_err() && !got_welcome,
                "PLAIN server authenticated a HELLO command with trailing credential bytes"
            );
        });
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
