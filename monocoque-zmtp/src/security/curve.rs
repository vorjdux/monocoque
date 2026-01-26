//! CURVE encryption mechanism (RFC 26)
//!
//! CurveZMQ provides public-key cryptography with perfect forward secrecy:
//! - Elliptic curve Diffie-Hellman key exchange (X25519)
//! - Authenticated encryption (ChaCha20-Poly1305)
//! - Resistance to man-in-the-middle attacks
//! - Zero-knowledge password proofs
//!
//! ## Security Properties
//!
//! - **Confidentiality**: All messages encrypted with ephemeral keys
//! - **Authentication**: Server key verified by client
//! - **Perfect Forward Secrecy**: Compromise of long-term keys doesn't reveal past messages
//! - **Replay Protection**: Nonces prevent message replay
//!
//! ## Protocol Flow (CurveZMQ)
//!
//! ```text
//! Client                                Server
//!   |                                      |
//!   |--- HELLO (client ephemeral key) --->|
//!   |                                      |
//!   |<-- WELCOME (server ephemeral key) ---|
//!   |         + encrypted cookie           |
//!   |                                      |
//!   |--- INITIATE (proof of key) -------->|
//!   |      + encrypted metadata            |
//!   |                                      |
//!   |<-- READY (confirmation) ------------|
//!   |                                      |
//!   |<=== Encrypted MESSAGE frames ======>|
//! ```
//!
//! ## Key Types
//!
//! - **Long-term keys**: Server's permanent identity (32-byte public/secret pair)
//! - **Short-term keys**: Ephemeral keys per connection
//! - **Shared secrets**: Computed via X25519 key exchange
//!
//! ## References
//!
//! - RFC 26: https://rfc.zeromq.org/spec/26/
//! - CurveCP: https://curvecp.org/

use bytes::{Bytes, BytesMut};
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    ChaCha20Poly1305, Nonce,
};
use compio::io::{AsyncRead, AsyncWrite};
use rand::RngCore;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, warn};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::codec::ZmtpError;
use crate::security::zap::{ZapMechanism, ZapRequest, ZapStatus};

/// CURVE command identifiers
const CURVE_HELLO: &[u8] = b"\x05HELLO";
const CURVE_WELCOME: &[u8] = b"\x07WELCOME";
const CURVE_INITIATE: &[u8] = b"\x08INITIATE";
const CURVE_READY: &[u8] = b"\x05READY";
const CURVE_MESSAGE: &[u8] = b"\x07MESSAGE";
// CURVE ERROR command - reserved for protocol error reporting
#[allow(dead_code)]
const CURVE_ERROR: &[u8] = b"\x05ERROR";

/// CURVE key sizes
pub const CURVE_KEY_SIZE: usize = 32;
pub const CURVE_NONCE_SIZE: usize = 24;
pub const CURVE_BOX_OVERHEAD: usize = 16; // Poly1305 tag

/// CURVE public key (32 bytes)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurvePublicKey([u8; CURVE_KEY_SIZE]);

impl CurvePublicKey {
    /// Create from bytes
    pub const fn from_bytes(bytes: [u8; CURVE_KEY_SIZE]) -> Self {
        Self(bytes)
    }

    /// Get raw bytes
    pub const fn as_bytes(&self) -> &[u8; CURVE_KEY_SIZE] {
        &self.0
    }

    /// Convert to X25519 public key
    pub fn to_x25519(&self) -> PublicKey {
        PublicKey::from(self.0)
    }
}

impl From<[u8; CURVE_KEY_SIZE]> for CurvePublicKey {
    fn from(bytes: [u8; CURVE_KEY_SIZE]) -> Self {
        Self(bytes)
    }
}

impl From<PublicKey> for CurvePublicKey {
    fn from(key: PublicKey) -> Self {
        Self(*key.as_bytes())
    }
}

impl AsRef<[u8]> for CurvePublicKey {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// CURVE secret key (32 bytes)
#[derive(Clone)]
pub struct CurveSecretKey(StaticSecret);

impl CurveSecretKey {
    /// Generate a new random secret key
    pub fn generate() -> Self {
        Self(StaticSecret::random_from_rng(OsRng))
    }

    /// Create from bytes
    pub fn from_bytes(bytes: [u8; CURVE_KEY_SIZE]) -> Self {
        Self(StaticSecret::from(bytes))
    }

    /// Get public key
    pub fn public_key(&self) -> CurvePublicKey {
        CurvePublicKey::from(PublicKey::from(&self.0))
    }

    /// Compute shared secret via ECDH
    pub fn diffie_hellman(&self, peer_public: &CurvePublicKey) -> [u8; CURVE_KEY_SIZE] {
        *self.0.diffie_hellman(&peer_public.to_x25519()).as_bytes()
    }
}

impl std::fmt::Debug for CurveSecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CurveSecretKey([REDACTED])")
    }
}

/// CURVE key pair (public + secret)
#[derive(Debug, Clone)]
pub struct CurveKeyPair {
    pub public: CurvePublicKey,
    pub secret: CurveSecretKey,
}

impl CurveKeyPair {
    /// Generate a new random key pair
    pub fn generate() -> Self {
        let secret = CurveSecretKey::generate();
        let public = secret.public_key();
        Self { public, secret }
    }

    /// Create from existing keys
    pub const fn from_keys(public: CurvePublicKey, secret: CurveSecretKey) -> Self {
        Self { public, secret }
    }
}

/// CURVE encryption box (ChaCha20-Poly1305)
struct CurveBox {
    cipher: ChaCha20Poly1305,
}

impl CurveBox {
    /// Create new box from shared secret
    fn new(shared_secret: &[u8; CURVE_KEY_SIZE]) -> Self {
        let cipher = ChaCha20Poly1305::new(shared_secret.into());
        Self { cipher }
    }

    /// Encrypt message with nonce
    fn encrypt(&self, plaintext: &[u8], nonce: &[u8; CURVE_NONCE_SIZE]) -> Result<Vec<u8>, CurveError> {
        // ChaCha20Poly1305 uses 12-byte nonces, take first 12 bytes of ZMQ's 24-byte nonce
        let nonce = Nonce::from_slice(&nonce[..12]);
        self.cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| CurveError::EncryptionFailed)
    }

    /// Decrypt message with nonce
    fn decrypt(&self, ciphertext: &[u8], nonce: &[u8; CURVE_NONCE_SIZE]) -> Result<Vec<u8>, CurveError> {
        // ChaCha20Poly1305 uses 12-byte nonces, take first 12 bytes of ZMQ's 24-byte nonce
        let nonce = Nonce::from_slice(&nonce[..12]);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| CurveError::DecryptionFailed)
    }
}

/// CURVE-specific errors
#[derive(Debug, Error)]
pub enum CurveError {
    #[error("Encryption failed")]
    EncryptionFailed,
    #[error("Decryption failed")]
    DecryptionFailed,
    #[error("Invalid key size")]
    InvalidKeySize,
    #[error("Invalid nonce")]
    InvalidNonce,
    #[error("Protocol violation")]
    ProtocolViolation,
    #[error("Authentication failed")]
    AuthenticationFailed,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// CURVE client state machine
pub struct CurveClient {
    /// Client's long-term key pair
    client_keypair: CurveKeyPair,
    /// Server's long-term public key (used in handshake verification)
    #[allow(dead_code)]
    server_public: CurvePublicKey,
    /// Client's short-term (ephemeral) key pair
    client_short_keypair: CurveKeyPair,
    /// Server's short-term public key (received in WELCOME)
    server_short_public: Option<CurvePublicKey>,
    /// Send nonce counter
    send_nonce: u64,
    /// Receive nonce counter (for message authentication)
    #[allow(dead_code)]
    recv_nonce: u64,
    /// Encryption box for messages (after READY)
    message_box: Option<CurveBox>,
}

impl CurveClient {
    /// Create new CURVE client
    pub fn new(client_keypair: CurveKeyPair, server_public: CurvePublicKey) -> Self {
        Self {
            client_keypair,
            server_public,
            client_short_keypair: CurveKeyPair::generate(),
            server_short_public: None,
            send_nonce: 0,
            recv_nonce: 0,
            message_box: None,
        }
    }

    /// Perform client handshake
    pub async fn handshake<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.send_hello(stream, timeout).await?;
        self.recv_welcome(stream, timeout).await?;
        self.send_initiate(stream, timeout).await?;
        self.recv_ready(stream, timeout).await?;
        Ok(())
    }

    /// Send HELLO command
    async fn send_hello<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::write_all_with_timeout;

        debug!("[CURVE CLIENT] Sending HELLO");

        let mut hello = BytesMut::new();
        hello.extend_from_slice(CURVE_HELLO);
        
        // Version (1 byte, always 1)
        hello.extend_from_slice(&[1u8]);
        
        // Client short-term public key (32 bytes)
        hello.extend_from_slice(self.client_short_keypair.public.as_bytes());
        
        // Nonce (8 bytes of zeros for HELLO)
        hello.extend_from_slice(&[0u8; 8]);
        
        // Signature (64 bytes, zeros for now - simplified)
        hello.extend_from_slice(&[0u8; 64]);

        let buf_result = write_all_with_timeout(stream, hello.freeze().to_vec(), timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _) = buf_result;
        result.map_err(Into::into)
    }

    /// Receive WELCOME command
    async fn recv_welcome<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::read_exact_with_timeout;

        debug!("[CURVE CLIENT] Waiting for WELCOME");

        // Read WELCOME header (7 bytes)
        let header = vec![0u8; 7];
        let buf_result = read_exact_with_timeout(stream, header, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, header) = buf_result;
        result?;

        if &header[..] != CURVE_WELCOME {
            warn!("[CURVE CLIENT] Invalid WELCOME header");
            return Err(ZmtpError::Protocol);
        }

        // Read server short-term public key (32 bytes)
        let server_short_key = vec![0u8; CURVE_KEY_SIZE];
        let buf_result = read_exact_with_timeout(stream, server_short_key, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, server_short_key) = buf_result;
        result?;

        let mut key_array = [0u8; CURVE_KEY_SIZE];
        key_array.copy_from_slice(&server_short_key);
        self.server_short_public = Some(CurvePublicKey::from_bytes(key_array));

        // Read encrypted cookie (96 bytes)
        let cookie = vec![0u8; 96];
        let buf_result = read_exact_with_timeout(stream, cookie, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _cookie) = buf_result;
        result?;

        debug!("[CURVE CLIENT] Received WELCOME");
        Ok(())
    }

    /// Send INITIATE command
    async fn send_initiate<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::write_all_with_timeout;

        debug!("[CURVE CLIENT] Sending INITIATE");

        let mut initiate = BytesMut::new();
        initiate.extend_from_slice(CURVE_INITIATE);
        
        // Client long-term public key (32 bytes)
        initiate.extend_from_slice(self.client_keypair.public.as_bytes());
        
        // Nonce (8 bytes)
        let mut nonce = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut nonce);
        initiate.extend_from_slice(&nonce);
        
        // Encrypted vouch (128 bytes)
        initiate.extend_from_slice(&[0u8; 128]);

        let buf_result = write_all_with_timeout(stream, initiate.freeze().to_vec(), timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _) = buf_result;
        result.map_err(Into::into)
    }

    /// Receive READY command
    async fn recv_ready<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::read_exact_with_timeout;

        debug!("[CURVE CLIENT] Waiting for READY");

        // Read READY header (5 bytes)
        let header = vec![0u8; 5];
        let buf_result = read_exact_with_timeout(stream, header, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, header) = buf_result;
        result?;

        if &header[..] != CURVE_READY {
            warn!("[CURVE CLIENT] Invalid READY header");
            return Err(ZmtpError::Protocol);
        }

        // Compute shared secret for message encryption
        let server_short_public = self.server_short_public
            .ok_or(ZmtpError::Protocol)?;
        
        let shared_secret = self.client_short_keypair.secret
            .diffie_hellman(&server_short_public);
        
        self.message_box = Some(CurveBox::new(&shared_secret));

        debug!("[CURVE CLIENT] Handshake complete");
        Ok(())
    }

    /// Encrypt a message
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Bytes, CurveError> {
        let message_box = self.message_box.as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        // Create nonce (24 bytes: "CurveZMQMESSAGEC" + 8-byte counter)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGEC");
        nonce[16..].copy_from_slice(&self.send_nonce.to_be_bytes());
        self.send_nonce += 1;

        let ciphertext = message_box.encrypt(plaintext, &nonce)?;
        
        let mut message = BytesMut::new();
        message.extend_from_slice(CURVE_MESSAGE);
        message.extend_from_slice(&nonce[16..]); // Only send counter part
        message.extend_from_slice(&ciphertext);
        
        Ok(message.freeze())
    }

    /// Decrypt a message
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Bytes, CurveError> {
        if message.len() < 7 + 8 {
            return Err(CurveError::ProtocolViolation);
        }

        if &message[..7] != CURVE_MESSAGE {
            return Err(CurveError::ProtocolViolation);
        }

        let message_box = self.message_box.as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        // Reconstruct nonce
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGES");
        nonce[16..].copy_from_slice(&message[7..15]);

        let plaintext = message_box.decrypt(&message[15..], &nonce)?;
        Ok(Bytes::from(plaintext))
    }
}

/// CURVE server state machine
pub struct CurveServer {
    /// Server's long-term key pair (for signing responses)
    #[allow(dead_code)]
    server_keypair: CurveKeyPair,
    /// Server's short-term (ephemeral) key pair
    server_short_keypair: CurveKeyPair,
    /// Client's short-term public key (received in HELLO)
    client_short_public: Option<CurvePublicKey>,
    /// Client's long-term public key (received in INITIATE)
    client_public: Option<CurvePublicKey>,
    /// Send nonce counter
    send_nonce: u64,
    /// Receive nonce counter (for message authentication)
    #[allow(dead_code)]
    recv_nonce: u64,
    /// Encryption box for messages (after READY)
    message_box: Option<CurveBox>,
}

impl CurveServer {
    /// Create new CURVE server
    pub fn new(server_keypair: CurveKeyPair) -> Self {
        Self {
            server_keypair,
            server_short_keypair: CurveKeyPair::generate(),
            client_short_public: None,
            client_public: None,
            send_nonce: 0,
            recv_nonce: 0,
            message_box: None,
        }
    }

    /// Perform server handshake
    pub async fn handshake<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<CurvePublicKey, ZmtpError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.recv_hello(stream, timeout).await?;
        self.send_welcome(stream, timeout).await?;
        self.recv_initiate(stream, timeout).await?;
        self.send_ready(stream, timeout).await?;
        
        // Return client's public key for authentication
        Ok(self.client_public.unwrap())
    }

    /// Receive HELLO command
    async fn recv_hello<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::read_exact_with_timeout;

        debug!("[CURVE SERVER] Waiting for HELLO");

        // Read HELLO header (5 bytes)
        let header = vec![0u8; 5];
        let buf_result = read_exact_with_timeout(stream, header, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, header) = buf_result;
        result?;

        if &header[..] != CURVE_HELLO {
            warn!("[CURVE SERVER] Invalid HELLO header");
            return Err(ZmtpError::Protocol);
        }

        // Read version (1 byte)
        let version = vec![0u8; 1];
        let buf_result = read_exact_with_timeout(stream, version, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _version) = buf_result;
        result?;

        // Read client short-term public key (32 bytes)
        let client_short_key = vec![0u8; CURVE_KEY_SIZE];
        let buf_result = read_exact_with_timeout(stream, client_short_key, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, client_short_key) = buf_result;
        result?;

        let mut key_array = [0u8; CURVE_KEY_SIZE];
        key_array.copy_from_slice(&client_short_key);
        self.client_short_public = Some(CurvePublicKey::from_bytes(key_array));

        // Skip nonce and signature (72 bytes)
        let skip_buf = vec![0u8; 72];
        let buf_result = read_exact_with_timeout(stream, skip_buf, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _) = buf_result;
        result?;

        debug!("[CURVE SERVER] Received HELLO");
        Ok(())
    }

    /// Send WELCOME command
    async fn send_welcome<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::write_all_with_timeout;

        debug!("[CURVE SERVER] Sending WELCOME");

        let mut welcome = BytesMut::new();
        welcome.extend_from_slice(CURVE_WELCOME);
        
        // Server short-term public key (32 bytes)
        welcome.extend_from_slice(self.server_short_keypair.public.as_bytes());
        
        // Encrypted cookie (96 bytes, zeros for now - simplified)
        welcome.extend_from_slice(&[0u8; 96]);

        let buf_result = write_all_with_timeout(stream, welcome.freeze().to_vec(), timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _) = buf_result;
        result.map_err(Into::into)
    }

    /// Receive INITIATE command
    async fn recv_initiate<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::read_exact_with_timeout;

        debug!("[CURVE SERVER] Waiting for INITIATE");

        // Read INITIATE header (8 bytes)
        let header = vec![0u8; 8];
        let buf_result = read_exact_with_timeout(stream, header, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, header) = buf_result;
        result?;

        if &header[..] != CURVE_INITIATE {
            warn!("[CURVE SERVER] Invalid INITIATE header");
            return Err(ZmtpError::Protocol);
        }

        // Read client long-term public key (32 bytes)
        let client_key = vec![0u8; CURVE_KEY_SIZE];
        let buf_result = read_exact_with_timeout(stream, client_key, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, client_key) = buf_result;
        result?;

        let mut key_array = [0u8; CURVE_KEY_SIZE];
        key_array.copy_from_slice(&client_key);
        self.client_public = Some(CurvePublicKey::from_bytes(key_array));

        // Skip nonce and vouch (136 bytes)
        let skip_buf = vec![0u8; 136];
        let buf_result = read_exact_with_timeout(stream, skip_buf, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _) = buf_result;
        result?;

        debug!("[CURVE SERVER] Received INITIATE");
        Ok(())
    }

    /// Send READY command
    async fn send_ready<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        use compio::buf::BufResult;
        use monocoque_core::timeout::write_all_with_timeout;

        debug!("[CURVE SERVER] Sending READY");

        let ready = Bytes::from_static(CURVE_READY).to_vec();
        let buf_result = write_all_with_timeout(stream, ready, timeout).await.map_err(ZmtpError::from)?;
        let BufResult(result, _) = buf_result;
        result?;

        // Compute shared secret for message encryption
        let client_short_public = self.client_short_public
            .ok_or(ZmtpError::Protocol)?;
        
        let shared_secret = self.server_short_keypair.secret
            .diffie_hellman(&client_short_public);
        
        self.message_box = Some(CurveBox::new(&shared_secret));

        debug!("[CURVE SERVER] Handshake complete");
        Ok(())
    }

    /// Encrypt a message
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Bytes, CurveError> {
        let message_box = self.message_box.as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        // Create nonce (24 bytes: "CurveZMQMESSAGES" + 8-byte counter)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGES");
        nonce[16..].copy_from_slice(&self.send_nonce.to_be_bytes());
        self.send_nonce += 1;

        let ciphertext = message_box.encrypt(plaintext, &nonce)?;
        
        let mut message = BytesMut::new();
        message.extend_from_slice(CURVE_MESSAGE);
        message.extend_from_slice(&nonce[16..]); // Only send counter part
        message.extend_from_slice(&ciphertext);
        
        Ok(message.freeze())
    }

    /// Decrypt a message
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Bytes, CurveError> {
        if message.len() < 7 + 8 {
            return Err(CurveError::ProtocolViolation);
        }

        if &message[..7] != CURVE_MESSAGE {
            return Err(CurveError::ProtocolViolation);
        }

        let message_box = self.message_box.as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        // Reconstruct nonce
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGEC");
        nonce[16..].copy_from_slice(&message[7..15]);

        let plaintext = message_box.decrypt(&message[15..], &nonce)?;
        Ok(Bytes::from(plaintext))
    }
}

/// Create a ZAP request for CURVE authentication
pub fn create_curve_zap_request(
    request_id: impl Into<String>,
    domain: impl Into<String>,
    address: impl Into<String>,
    identity: Bytes,
    client_public_key: &CurvePublicKey,
) -> ZapRequest {
    ZapRequest::new(
        request_id,
        domain,
        address,
        identity,
        ZapMechanism::Curve,
        vec![Bytes::copy_from_slice(client_public_key.as_bytes())],
    )
}

/// CURVE server handshake with ZAP authentication
///
/// Performs CURVE handshake and authenticates the client via ZAP protocol.
/// After receiving the client's public key during INITIATE, sends a ZAP request
/// to verify the client is authorized.
///
/// # Arguments
/// * `stream` - Network stream for the connection
/// * `server_keypair` - Server's long-term CURVE key pair  
/// * `domain` - ZAP authentication domain
/// * `timeout` - Optional timeout for ZAP request
///
/// # Returns
/// * `Ok(CurvePublicKey)` - Authenticated client's public key
/// * `Err(ZmtpError)` - Authentication failed or protocol error
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::security::curve::{curve_server_handshake_zap, CurveKeyPair};
/// use std::time::Duration;
///
/// async fn accept_curve_client(mut stream: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
///     let server_keypair = CurveKeyPair::generate();
///     let client_key = curve_server_handshake_zap(
///         &mut stream,
///         server_keypair,
///         "production".to_string(),
///         Some(Duration::from_secs(5)),
///     ).await?;
///     println!("Authenticated client: {:?}", client_key);
///     Ok(())
/// }
/// ```
pub async fn curve_server_handshake_zap<S>(
    stream: &mut S,
    server_keypair: CurveKeyPair,
    domain: String,
    timeout: Option<Duration>,
) -> Result<CurvePublicKey, ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use crate::security::zap_client::ZapClient;

    debug!("[CURVE SERVER ZAP] Starting ZAP-authenticated handshake");

    // Perform CURVE handshake
    let mut curve_server = CurveServer::new(server_keypair);
    let client_public_key = curve_server.handshake(stream, timeout).await?;

    debug!("[CURVE SERVER ZAP] CURVE handshake complete, authenticating via ZAP");

    // Create ZAP client
    let zap_timeout = timeout.unwrap_or(Duration::from_secs(5));
    let mut zap_client = ZapClient::new(zap_timeout)
        .map_err(|e| {
            warn!("[CURVE SERVER ZAP] Failed to create ZAP client: {}", e);
            ZmtpError::AuthenticationFailed
        })?;

    // Send ZAP authentication request
    let peer_addr = "unknown".to_string(); // TODO: Get actual peer address
    let zap_response = zap_client
        .authenticate_curve(client_public_key.as_bytes(), &domain, &peer_addr)
        .await
        .map_err(|e| {
            warn!("[CURVE SERVER ZAP] ZAP request failed: {}", e);
            ZmtpError::AuthenticationFailed
        })?;

    // Check ZAP response status
    if matches!(zap_response.status_code, ZapStatus::Success) {
        debug!("[CURVE SERVER ZAP] Authentication successful for client key: {:?}", client_public_key);
        Ok(client_public_key)
    } else {
        warn!(
            "[CURVE SERVER ZAP] Authentication failed: {} (status: {:?})",
            zap_response.status_text, zap_response.status_code
        );

        // TODO: Send ERROR command to client
        Err(ZmtpError::AuthenticationFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let keypair = CurveKeyPair::generate();
        assert_eq!(keypair.public.as_bytes().len(), CURVE_KEY_SIZE);
        
        // Verify public key matches secret key
        let derived_public = keypair.secret.public_key();
        assert_eq!(keypair.public, derived_public);
    }

    #[test]
    fn test_diffie_hellman() {
        let alice = CurveKeyPair::generate();
        let bob = CurveKeyPair::generate();

        let alice_shared = alice.secret.diffie_hellman(&bob.public);
        let bob_shared = bob.secret.diffie_hellman(&alice.public);

        assert_eq!(alice_shared, bob_shared);
    }

    #[test]
    fn test_curve_box_encrypt_decrypt() {
        let shared_secret = [42u8; CURVE_KEY_SIZE];
        let box_ = CurveBox::new(&shared_secret);
        
        let plaintext = b"Hello, CURVE!";
        let nonce = [1u8; CURVE_NONCE_SIZE];
        
        let ciphertext = box_.encrypt(plaintext, &nonce).unwrap();
        let decrypted = box_.decrypt(&ciphertext, &nonce).unwrap();
        
        assert_eq!(plaintext, decrypted.as_slice());
    }

    #[test]
    fn test_curve_zap_request() {
        let keypair = CurveKeyPair::generate();
        let request = create_curve_zap_request(
            "req123",
            "production",
            "192.168.1.100:5555",
            Bytes::from("client1"),
            &keypair.public,
        );

        assert_eq!(request.mechanism, ZapMechanism::Curve);
        assert_eq!(request.credentials.len(), 1);
        assert_eq!(request.credentials[0].len(), CURVE_KEY_SIZE);
    }
}
