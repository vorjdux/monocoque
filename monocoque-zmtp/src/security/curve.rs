//! CURVE encryption mechanism (RFC 26)
//!
//! CurveZMQ provides public-key cryptography with perfect forward secrecy:
//! - Elliptic curve Diffie-Hellman key exchange (X25519)
//! - Authenticated encryption (XSalsa20-Poly1305 for handshake, XChaCha20-Poly1305 for messages)
//! - Resistance to man-in-the-middle attacks
//! - Zero-knowledge proof of long-term key ownership via vouch
//!
//! ## Protocol Flow (CurveZMQ RFC 26)
//!
//! ```text
//! Client                                Server
//!   |                                      |
//!   |--- HELLO (c'.pk + box proof) ------>|
//!   |                                      |
//!   |<-- WELCOME (s'.pk + cookie) --------|
//!   |                                      |
//!   |--- INITIATE (cookie + vouch) ------>|
//!   |                                      |
//!   |<-- READY (server metadata box) -----|
//!   |                                      |
//!   |<=== Encrypted MESSAGE frames ======>|
//! ```
//!
//! All post-greeting messages are ZMTP frames (flag byte + length + body).
//! Metadata (Socket-Type, Identity) is exchanged inside the CURVE boxes.
//!
//! ## References
//!
//! - RFC 26: <https://rfc.zeromq.org/spec/26/>
//! - RFC 23 (ZMTP 3.1): <https://rfc.zeromq.org/spec/23/>

use bytes::{Bytes, BytesMut};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, OsRng},
};
use compio_io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use crypto_box::{
    PublicKey as SalsaPublicKey, SalsaBox, SecretKey as SalsaSecretKey,
    aead::generic_array::GenericArray,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, warn};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::codec::ZmtpError;
use crate::security::zap::{ZapMechanism, ZapRequest, ZapStatus};

/// CURVE command identifiers
const CURVE_HELLO: &[u8] = b"\x05HELLO";
const CURVE_WELCOME: &[u8] = b"\x07WELCOME";
const CURVE_INITIATE: &[u8] = b"\x08INITIATE";
const CURVE_READY: &[u8] = b"\x05READY";
const CURVE_MESSAGE: &[u8] = b"\x07MESSAGE";
const CURVE_MESSAGE_NONCE_SIZE: usize = 8;

/// Maximum body size for INITIATE (supports variable-length metadata).
const MAX_INITIATE_BODY: usize = 4096;
/// Maximum body size for other CURVE commands (HELLO, WELCOME, READY).
const MAX_CURVE_BODY: usize = 512;
/// 16-byte nonce prefix for CURVE READY: "READY" + 11 NULs
const CURVE_READY_NONCE_PREFIX: &[u8; 16] = b"READY\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";

/// CURVE key sizes
pub const CURVE_KEY_SIZE: usize = 32;
/// Size of a CURVE nonce in bytes.
pub const CURVE_NONCE_SIZE: usize = 24;
/// Overhead added by the Poly1305 authentication tag.
pub const CURVE_BOX_OVERHEAD: usize = 16;

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
        let shared = *self.0.diffie_hellman(&peer_public.to_x25519()).as_bytes();
        if shared == [0u8; CURVE_KEY_SIZE] {
            return [0xff; CURVE_KEY_SIZE];
        }
        shared
    }

    /// Return raw scalar bytes for use with crypto_box primitives
    fn to_raw_bytes(&self) -> [u8; CURVE_KEY_SIZE] {
        self.0.to_bytes()
    }
}

impl std::fmt::Debug for CurveSecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CurveSecretKey([REDACTED])")
    }
}

impl Drop for CurveSecretKey {
    fn drop(&mut self) {
        // Zeroize the raw scalar bytes before deallocation to prevent
        // secret key material from lingering in freed memory.
        let mut raw = self.0.to_bytes();
        raw.zeroize();
    }
}

/// CURVE key pair (public + secret)
#[derive(Debug, Clone)]
pub struct CurveKeyPair {
    /// Long-term public key.
    pub public: CurvePublicKey,
    /// Long-term secret key.
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

// ── NaCl box helpers ──────────────────────────────────────────────────────────

/// Encrypt using NaCl crypto_box (X25519 + XSalsa20-Poly1305).
///
/// `their_pk` and `my_sk` are raw 32-byte X25519 scalars.
/// `nonce_24` must be a 24-byte nonce (typically a prefix + counter/random).
fn salsa_encrypt(
    their_pk: &[u8; 32],
    my_sk: &[u8; 32],
    nonce_24: &[u8; 24],
    plaintext: &[u8],
) -> Result<Vec<u8>, CurveError> {
    let pk = SalsaPublicKey::from(*their_pk);
    let sk = SalsaSecretKey::from(*my_sk);
    let box_ = SalsaBox::new(&pk, &sk);
    let nonce = GenericArray::from(*nonce_24);
    box_.encrypt(&nonce, plaintext)
        .map_err(|_| CurveError::EncryptionFailed)
}

/// Decrypt using NaCl crypto_box (X25519 + XSalsa20-Poly1305).
fn salsa_decrypt(
    their_pk: &[u8; 32],
    my_sk: &[u8; 32],
    nonce_24: &[u8; 24],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CurveError> {
    let pk = SalsaPublicKey::from(*their_pk);
    let sk = SalsaSecretKey::from(*my_sk);
    let box_ = SalsaBox::new(&pk, &sk);
    let nonce = GenericArray::from(*nonce_24);
    box_.decrypt(&nonce, ciphertext)
        .map_err(|_| CurveError::DecryptionFailed)
}

// ── Message box (XChaCha20-Poly1305) ─────────────────────────────────────────

/// Post-handshake message encryption box (XChaCha20-Poly1305).
///
/// Keyed by SHA-256(c'·S ‖ C·s' ‖ c'·s') per the 3-way DH derivation.
pub(crate) struct CurveBox {
    cipher: XChaCha20Poly1305,
}

impl CurveBox {
    fn new(key: &[u8; CURVE_KEY_SIZE]) -> Self {
        let cipher = XChaCha20Poly1305::new(key.into());
        Self { cipher }
    }

    fn encrypt(
        &self,
        plaintext: &[u8],
        nonce: &[u8; CURVE_NONCE_SIZE],
    ) -> Result<Vec<u8>, CurveError> {
        let nonce = XNonce::from_slice(nonce);
        self.cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| CurveError::EncryptionFailed)
    }

    fn decrypt(
        &self,
        ciphertext: &[u8],
        nonce: &[u8; CURVE_NONCE_SIZE],
    ) -> Result<Vec<u8>, CurveError> {
        let nonce = XNonce::from_slice(nonce);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| CurveError::DecryptionFailed)
    }
}

// ── Message parsing ───────────────────────────────────────────────────────────

struct CurveMessageParts<'a> {
    short_nonce: &'a [u8],
    ciphertext: &'a [u8],
}

#[inline]
fn parse_curve_message(message: &[u8]) -> Result<CurveMessageParts<'_>, CurveError> {
    let command_len = CURVE_MESSAGE.len();
    if message.len() < command_len + CURVE_MESSAGE_NONCE_SIZE {
        return Err(CurveError::ProtocolViolation);
    }
    if &message[..command_len] != CURVE_MESSAGE {
        return Err(CurveError::ProtocolViolation);
    }
    Ok(CurveMessageParts {
        short_nonce: &message[command_len..command_len + CURVE_MESSAGE_NONCE_SIZE],
        ciphertext: &message[command_len + CURVE_MESSAGE_NONCE_SIZE..],
    })
}

// ── Key derivation ────────────────────────────────────────────────────────────

/// Derive the post-handshake message key via SHA-256 of the three DH legs.
///
/// `dh1` = c'·S, `dh2` = C·s', `dh3` = c'·s'.
fn derive_message_key(dh1: &[u8; 32], dh2: &[u8; 32], dh3: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(dh1);
    h.update(dh2);
    h.update(dh3);
    h.finalize().into()
}

// ── ZMTP frame helpers ────────────────────────────────────────────────────────

/// Read one ZMTP command frame and return its body.
/// Rejects data frames (command flag 0x04 must be set).
async fn read_zmtp_cmd<S>(
    stream: &mut S,
    timeout: Option<Duration>,
    max_body: usize,
) -> Result<Vec<u8>, ZmtpError>
where
    S: AsyncRead + Unpin,
{
    use compio_buf::BufResult;
    use monocoque_core::timeout::read_exact_with_timeout;

    let BufResult(r, flags_buf) = read_exact_with_timeout(stream, [0u8; 1], timeout)
        .await
        .map_err(ZmtpError::from)?;
    r?;
    let flags = flags_buf[0];

    if flags & 0x04 == 0 {
        warn!(
            "Expected ZMTP command frame, got data frame (flags=0x{:02x})",
            flags
        );
        return Err(ZmtpError::Protocol);
    }

    let body_len: usize = if flags & 0x02 != 0 {
        let BufResult(r, len_buf) = read_exact_with_timeout(stream, [0u8; 8], timeout)
            .await
            .map_err(ZmtpError::from)?;
        r?;
        let raw_len = u64::from_be_bytes(len_buf);
        if raw_len > usize::MAX as u64 {
            warn!("ZMTP long-frame length overflows usize: {}", raw_len);
            return Err(ZmtpError::Protocol);
        }
        raw_len as usize
    } else {
        let BufResult(r, len_buf) = read_exact_with_timeout(stream, [0u8; 1], timeout)
            .await
            .map_err(ZmtpError::from)?;
        r?;
        len_buf[0] as usize
    };

    if body_len > max_body {
        warn!("ZMTP command body too large: {} > {}", body_len, max_body);
        return Err(ZmtpError::Protocol);
    }

    let BufResult(r, body) = read_exact_with_timeout(stream, vec![0u8; body_len], timeout)
        .await
        .map_err(ZmtpError::from)?;
    r?;
    Ok(body)
}

/// Write a slice as a ZMTP command frame (flag + length + body).
async fn write_zmtp_cmd<S>(
    stream: &mut S,
    body: &[u8],
    timeout: Option<Duration>,
) -> Result<(), ZmtpError>
where
    S: AsyncWrite + Unpin,
{
    use compio_buf::BufResult;
    use monocoque_core::timeout::write_all_with_timeout;

    let len = body.len();
    let mut frame = BytesMut::with_capacity(if len <= 255 { 2 + len } else { 9 + len });
    if len <= 255 {
        frame.extend_from_slice(&[0x04, len as u8]);
    } else {
        frame.extend_from_slice(&[0x06]);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    frame.extend_from_slice(body);

    let BufResult(r, _) = write_all_with_timeout(stream, frame.freeze().to_vec(), timeout)
        .await
        .map_err(ZmtpError::from)?;
    r.map_err(Into::into)
}

// ── ZMTP property helpers ─────────────────────────────────────────────────────

/// Encode Socket-Type and optional Identity as ZMTP property bytes (RFC 23 §2.5).
fn encode_zmtp_props(socket_type: &str, identity: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    push_prop(&mut out, b"Socket-Type", socket_type.as_bytes());
    if let Some(id) = identity {
        if !id.is_empty() {
            push_prop(&mut out, b"Identity", id);
        }
    }
    out
}

#[inline]
fn push_prop(out: &mut Vec<u8>, key: &[u8], value: &[u8]) {
    debug_assert!(key.len() <= 255);
    out.push(key.len() as u8);
    out.extend_from_slice(key);
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

/// Decode ZMTP property bytes, returning (socket_type, identity).
fn decode_zmtp_props(mut data: &[u8]) -> Result<(Option<Bytes>, Option<Bytes>), ZmtpError> {
    let mut socket_type = None;
    let mut identity = None;

    while !data.is_empty() {
        if data.len() < 5 {
            warn!(
                "[CURVE] ZMTP property list truncated (remaining {} bytes)",
                data.len()
            );
            return Err(ZmtpError::Protocol);
        }
        let klen = data[0] as usize;
        data = &data[1..];
        if data.len() < klen + 4 {
            warn!("[CURVE] ZMTP property key truncated (klen={})", klen);
            return Err(ZmtpError::Protocol);
        }
        let key = &data[..klen];
        data = &data[klen..];
        let vlen = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        data = &data[4..];
        if data.len() < vlen {
            warn!("[CURVE] ZMTP property value truncated (vlen={})", vlen);
            return Err(ZmtpError::Protocol);
        }
        let value = Bytes::copy_from_slice(&data[..vlen]);
        data = &data[vlen..];

        if key.eq_ignore_ascii_case(b"Socket-Type") {
            if socket_type.is_some() {
                return Err(ZmtpError::Protocol);
            }
            socket_type = Some(value);
        } else if key.eq_ignore_ascii_case(b"Identity") {
            if identity.is_some() {
                return Err(ZmtpError::Protocol);
            }
            if vlen > 255 {
                warn!("[CURVE] Identity property exceeds 255 bytes ({})", vlen);
                return Err(ZmtpError::Protocol);
            }
            identity = Some(value);
        }
    }

    Ok((socket_type, identity))
}

// ── Post-handshake message cipher ────────────────────────────────────────────

/// Post-handshake CURVE message cipher. Held by the socket after handshake completes.
pub struct CurveMessageCipher {
    cipher: CurveBox,
    send_nonce: u64,
    recv_nonce: u64,
    /// true = this side is the client (sends with MESSAGEC prefix)
    is_client: bool,
}

impl CurveMessageCipher {
    pub(crate) fn new_client(cipher: CurveBox, send_nonce: u64, recv_nonce: u64) -> Self {
        Self {
            cipher,
            send_nonce,
            recv_nonce,
            is_client: true,
        }
    }
    pub(crate) fn new_server(cipher: CurveBox, send_nonce: u64, recv_nonce: u64) -> Self {
        Self {
            cipher,
            send_nonce,
            recv_nonce,
            is_client: false,
        }
    }

    /// Returns true if `bytes` starts with the CURVE MESSAGE command name.
    pub fn is_curve_message(bytes: &[u8]) -> bool {
        bytes.starts_with(CURVE_MESSAGE)
    }

    /// Encrypt one ZMQ frame. `payload` is the raw frame data; `more` is the MORE flag.
    /// Returns the CURVE MESSAGE command body (starting with `\x07MESSAGE`), ready to be
    /// wrapped in a ZMTP command frame.
    pub fn encrypt_frame(&mut self, payload: &[u8], more: bool) -> Result<Bytes, CurveError> {
        let prefix: &[u8; 16] = if self.is_client {
            b"CurveZMQMESSAGEC"
        } else {
            b"CurveZMQMESSAGES"
        };
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(prefix);
        nonce[16..].copy_from_slice(&self.send_nonce.to_be_bytes());
        self.send_nonce = self
            .send_nonce
            .checked_add(1)
            .ok_or(CurveError::ProtocolViolation)?;

        let mut pt = Vec::with_capacity(1 + payload.len());
        pt.push(u8::from(more));
        pt.extend_from_slice(payload);

        let ciphertext = self.cipher.encrypt(&pt, &nonce)?;

        let body_len = CURVE_MESSAGE.len() + 8 + ciphertext.len();
        let mut body = BytesMut::with_capacity(body_len);
        body.extend_from_slice(CURVE_MESSAGE);
        body.extend_from_slice(&nonce[16..]);
        body.extend_from_slice(&ciphertext);
        Ok(body.freeze())
    }

    /// Decrypt a CURVE MESSAGE command body. `cmd_body` starts with `\x07MESSAGE`.
    /// Returns (more_flag, payload).
    pub fn decrypt_frame(&mut self, cmd_body: &[u8]) -> Result<(bool, Bytes), CurveError> {
        let parts = parse_curve_message(cmd_body)?;
        let counter = u64::from_be_bytes(
            parts
                .short_nonce
                .try_into()
                .map_err(|_| CurveError::InvalidNonce)?,
        );
        if counter < self.recv_nonce {
            return Err(CurveError::ProtocolViolation);
        }
        self.recv_nonce = counter
            .checked_add(1)
            .ok_or(CurveError::ProtocolViolation)?;

        let prefix: &[u8; 16] = if self.is_client {
            b"CurveZMQMESSAGES"
        } else {
            b"CurveZMQMESSAGEC"
        };
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(prefix);
        nonce[16..].copy_from_slice(parts.short_nonce);

        let plaintext = self.cipher.decrypt(parts.ciphertext, &nonce)?;
        if plaintext.is_empty() {
            return Err(CurveError::ProtocolViolation);
        }
        let more = (plaintext[0] & 0x01) != 0;
        Ok((more, Bytes::from(plaintext[1..].to_vec())))
    }
}

impl std::fmt::Debug for CurveMessageCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurveMessageCipher")
            .field("send_nonce", &self.send_nonce)
            .field("recv_nonce", &self.recv_nonce)
            .field("is_client", &self.is_client)
            .finish_non_exhaustive()
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// CURVE-specific errors
#[derive(Debug, Error)]
pub enum CurveError {
    /// Symmetric encryption failed.
    #[error("Encryption failed")]
    EncryptionFailed,
    /// Symmetric decryption or authentication-tag verification failed.
    #[error("Decryption failed")]
    DecryptionFailed,
    /// A key did not have the expected length.
    #[error("Invalid key size")]
    InvalidKeySize,
    /// A nonce had an unexpected format or length.
    #[error("Invalid nonce")]
    InvalidNonce,
    /// The peer violated the CurveZMQ protocol.
    #[error("Protocol violation")]
    ProtocolViolation,
    /// The peer's identity could not be verified.
    #[error("Authentication failed")]
    AuthenticationFailed,
    /// An underlying I/O error occurred.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Peer metadata returned after a completed CurveZMQ handshake.
pub struct CurveHandshakeResult {
    /// Raw peer socket-type bytes (e.g. `b"DEALER"`).
    pub peer_socket_type: Bytes,
    /// Peer identity, if announced.
    pub peer_identity: Option<Bytes>,
    /// Client's authenticated long-term public key (populated server-side only).
    pub peer_public_key: Option<CurvePublicKey>,
    /// Post-handshake cipher for encrypting/decrypting application messages.
    pub cipher: Option<CurveMessageCipher>,
}

// ── CurveClient ───────────────────────────────────────────────────────────────

/// CURVE client state machine
pub struct CurveClient {
    /// Client's long-term key pair (C / c)
    client_keypair: CurveKeyPair,
    /// Server's long-term public key (S)
    server_public: CurvePublicKey,
    /// Client's short-term (ephemeral) key pair (C' / c')
    client_short_keypair: CurveKeyPair,
    /// Server's short-term public key received in WELCOME (s')
    server_short_public: Option<CurvePublicKey>,
    /// Cookie received in WELCOME, echoed back in INITIATE (96 bytes)
    cookie: Option<Vec<u8>>,
    /// Local socket type to announce in INITIATE metadata
    local_socket_type: String,
    /// Local identity to announce in INITIATE metadata
    local_identity: Option<Bytes>,
    /// Peer socket type received in READY metadata
    peer_socket_type: Option<Bytes>,
    /// Peer identity received in READY metadata
    peer_identity_recv: Option<Bytes>,
    /// Send nonce counter
    send_nonce: u64,
    /// Next expected receive nonce counter (replay protection)
    recv_nonce: u64,
    /// Encryption box for messages (after READY)
    message_box: Option<CurveBox>,
}

impl CurveClient {
    /// Create new CURVE client
    pub fn new(
        client_keypair: CurveKeyPair,
        server_public: CurvePublicKey,
        local_socket_type: impl Into<String>,
        local_identity: Option<Bytes>,
    ) -> Self {
        Self {
            client_keypair,
            server_public,
            client_short_keypair: CurveKeyPair::generate(),
            server_short_public: None,
            cookie: None,
            local_socket_type: local_socket_type.into(),
            local_identity,
            peer_socket_type: None,
            peer_identity_recv: None,
            send_nonce: 1,
            recv_nonce: 1,
            message_box: None,
        }
    }

    /// Perform full client handshake: HELLO → WELCOME → INITIATE → READY
    pub async fn handshake<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<CurveHandshakeResult, ZmtpError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.send_hello(stream, timeout).await?;
        self.recv_welcome(stream, timeout).await?;
        self.send_initiate(stream, timeout).await?;
        self.recv_ready(stream, timeout).await?;
        let message_box = self.message_box.take().ok_or(ZmtpError::Protocol)?;
        let cipher = CurveMessageCipher::new_client(message_box, self.send_nonce, self.recv_nonce);
        Ok(CurveHandshakeResult {
            peer_socket_type: self.peer_socket_type.clone().ok_or(ZmtpError::Protocol)?,
            peer_identity: self.peer_identity_recv.clone(),
            peer_public_key: None,
            cipher: Some(cipher),
        })
    }

    /// Send HELLO command wrapped in a ZMTP command frame.
    ///
    /// Body: version(1) + padding(71) + c'.pk(32) + nonce_suffix(8) + hello_box(80) = 198 bytes
    ///
    /// hello_box = SalsaBox(c'→S).encrypt([0u8;64], "CurveZMQHELLO---" ‖ nonce_suffix)
    async fn send_hello<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        debug!("[CURVE CLIENT] Sending HELLO");

        let nonce_counter: u64 = 1;
        let mut nonce_24 = [0u8; 24];
        nonce_24[..16].copy_from_slice(b"CurveZMQHELLO---");
        nonce_24[16..].copy_from_slice(&nonce_counter.to_be_bytes());

        let hello_box = salsa_encrypt(
            self.server_public.as_bytes(),
            &self.client_short_keypair.secret.to_raw_bytes(),
            &nonce_24,
            &[0u8; 64],
        )
        .map_err(|_| ZmtpError::Protocol)?;
        // hello_box = 64 + 16 = 80 bytes

        let mut frame = BytesMut::with_capacity(198);
        frame.extend_from_slice(CURVE_HELLO); //  6
        frame.extend_from_slice(&[1u8]); //  1  version
        frame.extend_from_slice(&[0u8; 71]); // 71  padding
        frame.extend_from_slice(self.client_short_keypair.public.as_bytes()); // 32  c'.pk
        frame.extend_from_slice(&nonce_counter.to_be_bytes()); //  8  nonce
        frame.extend_from_slice(&hello_box); // 80  box
        // total = 198

        write_zmtp_cmd(stream, &frame, timeout).await
    }

    /// Receive and decrypt WELCOME command from a ZMTP frame.
    ///
    /// Body: \x07WELCOME(8) + server_nonce(16) + welcome_box(144) = 168 bytes
    ///
    /// welcome_box decrypts to: s'.pk(32) + cookie(96)
    async fn recv_welcome<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        debug!("[CURVE CLIENT] Waiting for WELCOME");

        let body = read_zmtp_cmd(stream, timeout, MAX_CURVE_BODY).await?;
        if body.len() != 168 || &body[..8] != CURVE_WELCOME {
            warn!("[CURVE CLIENT] Invalid WELCOME frame (len={})", body.len());
            return Err(ZmtpError::Protocol);
        }

        let server_nonce_16 = &body[8..24];
        let welcome_box = &body[24..168];

        // Decrypt: SalsaBox(S→c') using c'_sk + S_pk
        let mut nonce_24 = [0u8; 24];
        nonce_24[..8].copy_from_slice(b"WELCOME-");
        nonce_24[8..].copy_from_slice(server_nonce_16);

        let plaintext = salsa_decrypt(
            self.server_public.as_bytes(),
            &self.client_short_keypair.secret.to_raw_bytes(),
            &nonce_24,
            welcome_box,
        )
        .map_err(|_| {
            warn!("[CURVE CLIENT] Failed to decrypt WELCOME box");
            ZmtpError::Protocol
        })?;

        // plaintext = s'.pk(32) + cookie(96) = 128 bytes
        if plaintext.len() != 128 {
            warn!(
                "[CURVE CLIENT] WELCOME plaintext wrong size: {}",
                plaintext.len()
            );
            return Err(ZmtpError::Protocol);
        }

        let mut s_prime_pk = [0u8; 32];
        s_prime_pk.copy_from_slice(&plaintext[..32]);
        self.server_short_public = Some(CurvePublicKey::from_bytes(s_prime_pk));
        self.cookie = Some(plaintext[32..128].to_vec());

        debug!("[CURVE CLIENT] Received WELCOME, stored s'.pk and cookie");
        Ok(())
    }

    /// Send INITIATE command wrapped in a ZMTP command frame.
    ///
    /// Body: cookie(96) + C(32) + nonce_suffix(8) + initiate_box(variable)
    ///
    /// initiate_box = SalsaBox(c'→s').encrypt(vouch ‖ metadata, "CurveZMQINITIATE" ‖ nonce_suffix)
    /// vouch = vouch_nonce_16(16) + SalsaBox(C→S).encrypt(c'.pk ‖ s'.pk, "VOUCH---" ‖ vouch_nonce_16)
    #[allow(clippy::similar_names)]
    async fn send_initiate<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        debug!("[CURVE CLIENT] Sending INITIATE");

        let server_short_public = self.server_short_public.ok_or(ZmtpError::Protocol)?;
        let cookie = self.cookie.as_ref().ok_or(ZmtpError::Protocol)?;

        // Build vouch: Box[c'.pk ‖ s'.pk](C→S)
        let mut vouch_nonce_16 = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut vouch_nonce_16);
        let mut vouch_nonce_24 = [0u8; 24];
        vouch_nonce_24[..8].copy_from_slice(b"VOUCH---");
        vouch_nonce_24[8..].copy_from_slice(&vouch_nonce_16);

        let mut vouch_pt = [0u8; 64];
        vouch_pt[..32].copy_from_slice(self.client_short_keypair.public.as_bytes()); // c'.pk
        vouch_pt[32..].copy_from_slice(server_short_public.as_bytes()); // s'.pk

        let vouch_ct = salsa_encrypt(
            self.server_public.as_bytes(),
            &self.client_keypair.secret.to_raw_bytes(),
            &vouch_nonce_24,
            &vouch_pt,
        )
        .map_err(|_| ZmtpError::Protocol)?;
        // vouch_ct = 64 + 16 = 80 bytes

        // vouch = vouch_nonce_16(16) + vouch_ct(80) = 96 bytes
        let mut vouch = Vec::with_capacity(96);
        vouch.extend_from_slice(&vouch_nonce_16);
        vouch.extend_from_slice(&vouch_ct);

        // Append ZMTP properties (Socket-Type + optional Identity)
        let local_props =
            encode_zmtp_props(&self.local_socket_type, self.local_identity.as_deref());
        let mut initiate_pt = Vec::with_capacity(96 + local_props.len());
        initiate_pt.extend_from_slice(&vouch);
        initiate_pt.extend_from_slice(&local_props);

        // Build initiate_box: Box[vouch ‖ metadata](c'→s')
        let initiate_counter: u64 = 1;
        let mut initiate_nonce_24 = [0u8; 24];
        initiate_nonce_24[..16].copy_from_slice(b"CurveZMQINITIATE");
        initiate_nonce_24[16..].copy_from_slice(&initiate_counter.to_be_bytes());

        let initiate_box = salsa_encrypt(
            server_short_public.as_bytes(),
            &self.client_short_keypair.secret.to_raw_bytes(),
            &initiate_nonce_24,
            &initiate_pt,
        )
        .map_err(|_| ZmtpError::Protocol)?;

        let mut frame = BytesMut::with_capacity(9 + 96 + 32 + 8 + initiate_box.len());
        frame.extend_from_slice(CURVE_INITIATE); //   9
        frame.extend_from_slice(cookie); //  96
        frame.extend_from_slice(self.client_keypair.public.as_bytes()); //  32  C
        frame.extend_from_slice(&initiate_counter.to_be_bytes()); //   8
        frame.extend_from_slice(&initiate_box); // variable

        write_zmtp_cmd(stream, &frame, timeout).await
    }

    /// Receive READY command from a ZMTP frame, decrypt metadata, derive message key.
    async fn recv_ready<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        debug!("[CURVE CLIENT] Waiting for READY");

        let body = read_zmtp_cmd(stream, timeout, MAX_CURVE_BODY).await?;
        // body = \x05READY (6) + nonce_8 (8) + ready_box (variable)
        if body.len() < 30 || &body[..6] != CURVE_READY {
            warn!(
                "[CURVE CLIENT] Invalid CURVE READY frame (len={})",
                body.len()
            );
            return Err(ZmtpError::Protocol);
        }

        let nonce_8 = &body[6..14];
        let ready_box = &body[14..];

        let s_prime = self.server_short_public.ok_or(ZmtpError::Protocol)?;
        let mut ready_nonce_24 = [0u8; 24];
        ready_nonce_24[..16].copy_from_slice(CURVE_READY_NONCE_PREFIX);
        ready_nonce_24[16..].copy_from_slice(nonce_8);

        let metadata_bytes = salsa_decrypt(
            s_prime.as_bytes(),
            &self.client_short_keypair.secret.to_raw_bytes(),
            &ready_nonce_24,
            ready_box,
        )
        .map_err(|_| {
            warn!("[CURVE CLIENT] CURVE READY box decryption failed");
            ZmtpError::Protocol
        })?;

        let (peer_st, peer_id) = decode_zmtp_props(&metadata_bytes)?;
        if peer_st.is_none() {
            warn!("[CURVE CLIENT] CURVE READY missing Socket-Type");
            return Err(ZmtpError::Protocol);
        }
        self.peer_socket_type = peer_st;
        self.peer_identity_recv = peer_id;

        // Derive message key: SHA-256(c'·S ‖ C·s' ‖ c'·s')
        let dh1 = self
            .client_short_keypair
            .secret
            .diffie_hellman(&self.server_public); // c'·S
        let dh2 = self.client_keypair.secret.diffie_hellman(&s_prime); // C·s'
        let dh3 = self.client_short_keypair.secret.diffie_hellman(&s_prime); // c'·s'

        let key = derive_message_key(&dh1, &dh2, &dh3);
        self.message_box = Some(CurveBox::new(&key));

        debug!("[CURVE CLIENT] Handshake complete");
        Ok(())
    }

    /// Encrypt a message for the server
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Bytes, CurveError> {
        let message_box = self
            .message_box
            .as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        // Nonce = "CurveZMQMESSAGEC" + 8-byte counter (client→server)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGEC");
        nonce[16..].copy_from_slice(&self.send_nonce.to_be_bytes());
        self.send_nonce = self
            .send_nonce
            .checked_add(1)
            .ok_or(CurveError::ProtocolViolation)?;

        let ciphertext = message_box.encrypt(plaintext, &nonce)?;

        let mut message = BytesMut::with_capacity(CURVE_MESSAGE.len() + 8 + ciphertext.len());
        message.extend_from_slice(CURVE_MESSAGE);
        message.extend_from_slice(&nonce[16..]); // 8-byte counter suffix only
        message.extend_from_slice(&ciphertext);

        Ok(message.freeze())
    }

    /// Decrypt a message from the server
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Bytes, CurveError> {
        let parts = parse_curve_message(message)?;
        let message_box = self
            .message_box
            .as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        let counter = u64::from_be_bytes(
            parts
                .short_nonce
                .try_into()
                .map_err(|_| CurveError::InvalidNonce)?,
        );
        if counter < self.recv_nonce {
            return Err(CurveError::ProtocolViolation); // replay
        }
        self.recv_nonce = counter
            .checked_add(1)
            .ok_or(CurveError::ProtocolViolation)?;

        // Reconstruct full 24-byte nonce (server→client direction)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGES");
        nonce[16..].copy_from_slice(parts.short_nonce);

        let plaintext = message_box.decrypt(parts.ciphertext, &nonce)?;
        Ok(Bytes::from(plaintext))
    }
}

// ── CurveServer ───────────────────────────────────────────────────────────────

/// CURVE server state machine
pub struct CurveServer {
    /// Server's long-term key pair (S / s)
    server_keypair: CurveKeyPair,
    /// Server's short-term (ephemeral) key pair (S' / s')
    server_short_keypair: CurveKeyPair,
    /// Client's short-term public key received in HELLO (c')
    client_short_public: Option<CurvePublicKey>,
    /// Client's long-term public key received in INITIATE (C)
    client_public: Option<CurvePublicKey>,
    /// Symmetric key for cookie encryption/decryption (32 bytes, random per-server)
    cookie_key: [u8; 32],
    /// Local socket type to announce in READY metadata
    local_socket_type: String,
    /// Peer socket type received in INITIATE metadata
    peer_socket_type: Option<Bytes>,
    /// Peer identity received in INITIATE metadata
    peer_identity_recv: Option<Bytes>,
    /// Send nonce counter
    send_nonce: u64,
    /// Next expected receive nonce counter (replay protection)
    recv_nonce: u64,
    /// Encryption box for messages (after READY)
    message_box: Option<CurveBox>,
}

impl CurveServer {
    /// Create new CURVE server
    pub fn new(server_keypair: CurveKeyPair, local_socket_type: impl Into<String>) -> Self {
        let mut cookie_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut cookie_key);
        Self {
            server_keypair,
            server_short_keypair: CurveKeyPair::generate(),
            client_short_public: None,
            client_public: None,
            cookie_key,
            local_socket_type: local_socket_type.into(),
            peer_socket_type: None,
            peer_identity_recv: None,
            send_nonce: 1,
            recv_nonce: 1,
            message_box: None,
        }
    }

    /// Perform full server handshake: HELLO → WELCOME → INITIATE → READY
    pub async fn handshake<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<CurveHandshakeResult, ZmtpError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.recv_hello(stream, timeout).await?;
        self.send_welcome(stream, timeout).await?;
        self.recv_initiate(stream, timeout).await?;
        self.send_ready(stream, timeout).await?;
        let message_box = self.message_box.take().ok_or(ZmtpError::Protocol)?;
        let cipher = CurveMessageCipher::new_server(message_box, self.send_nonce, self.recv_nonce);
        Ok(CurveHandshakeResult {
            peer_socket_type: self.peer_socket_type.clone().ok_or(ZmtpError::Protocol)?,
            peer_identity: self.peer_identity_recv.clone(),
            peer_public_key: self.client_public,
            cipher: Some(cipher),
        })
    }

    /// Receive and verify HELLO command from a ZMTP frame.
    async fn recv_hello<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        debug!("[CURVE SERVER] Waiting for HELLO");

        let body = read_zmtp_cmd(stream, timeout, MAX_CURVE_BODY).await?;
        // body = \x05HELLO(6) + version(1) + padding(71) + c'.pk(32) + nonce_8(8) + hello_box(80) = 198
        if body.len() != 198 || &body[..6] != CURVE_HELLO {
            warn!("[CURVE SERVER] Invalid HELLO frame (len={})", body.len());
            return Err(ZmtpError::Protocol);
        }
        if body[6] != 1 {
            warn!("[CURVE SERVER] Unsupported HELLO version: {}", body[6]);
            return Err(ZmtpError::Protocol);
        }

        // c'.pk at [78..110], nonce_8 at [110..118], hello_box at [118..198]
        let c_prime_pk_bytes: [u8; 32] = body[78..110].try_into().unwrap();
        let nonce_suffix = &body[110..118];
        let hello_box = &body[118..198];

        // Validate nonce >= 1 (RFC 26 §5.2)
        if u64::from_be_bytes(nonce_suffix.try_into().unwrap_or([0u8; 8])) == 0 {
            warn!("[CURVE SERVER] HELLO nonce counter is 0, must be >= 1");
            return Err(ZmtpError::Protocol);
        }

        self.client_short_public = Some(CurvePublicKey::from_bytes(c_prime_pk_bytes));

        let mut nonce_24 = [0u8; 24];
        nonce_24[..16].copy_from_slice(b"CurveZMQHELLO---");
        nonce_24[16..].copy_from_slice(nonce_suffix);

        let plaintext = salsa_decrypt(
            &c_prime_pk_bytes,
            &self.server_keypair.secret.to_raw_bytes(),
            &nonce_24,
            hello_box,
        )
        .map_err(|_| {
            warn!("[CURVE SERVER] HELLO box authentication failed");
            ZmtpError::AuthenticationFailed
        })?;

        if plaintext.len() != 64 || plaintext.iter().any(|&b| b != 0) {
            warn!("[CURVE SERVER] HELLO box plaintext is not 64 zeros");
            return Err(ZmtpError::AuthenticationFailed);
        }

        debug!("[CURVE SERVER] HELLO verified");
        Ok(())
    }

    /// Build and send WELCOME command wrapped in a ZMTP command frame.
    ///
    /// Body: \x07WELCOME(8) + server_nonce_16(16) + welcome_box(144) = 168 bytes
    ///
    /// Cookie = cookie_nonce_16(16) + XChaCha20.encrypt(c'.pk ‖ s'.sk, "COOKIE--" ‖ cookie_nonce_16)
    #[allow(clippy::similar_names)]
    async fn send_welcome<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        debug!("[CURVE SERVER] Sending WELCOME");

        let c_prime_pk = self.client_short_public.ok_or(ZmtpError::Protocol)?;

        // Build cookie (96 bytes): cookie_nonce_16 + XChaCha20ct(c'.pk ‖ s'.sk_bytes)
        let mut cookie_nonce_16 = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut cookie_nonce_16);
        let mut cookie_nonce_24 = [0u8; 24];
        cookie_nonce_24[..8].copy_from_slice(b"COOKIE--");
        cookie_nonce_24[8..].copy_from_slice(&cookie_nonce_16);

        let mut cookie_pt = [0u8; 64];
        cookie_pt[..32].copy_from_slice(c_prime_pk.as_bytes());
        cookie_pt[32..].copy_from_slice(&self.server_short_keypair.secret.to_raw_bytes());

        let cookie_cipher = XChaCha20Poly1305::new(self.cookie_key.as_ref().into());
        let cookie_ct = cookie_cipher
            .encrypt(XNonce::from_slice(&cookie_nonce_24), cookie_pt.as_ref())
            .map_err(|_| ZmtpError::Protocol)?;
        // cookie_ct = 64 + 16 = 80 bytes

        let mut cookie = Vec::with_capacity(96);
        cookie.extend_from_slice(&cookie_nonce_16); // 16
        cookie.extend_from_slice(&cookie_ct); // 80
        // cookie = 96 bytes

        // Build welcome_box (144 bytes): SalsaBox(S→c').encrypt(s'.pk ‖ cookie)
        let mut server_nonce_16 = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut server_nonce_16);
        let mut welcome_nonce_24 = [0u8; 24];
        welcome_nonce_24[..8].copy_from_slice(b"WELCOME-");
        welcome_nonce_24[8..].copy_from_slice(&server_nonce_16);

        let mut welcome_pt = Vec::with_capacity(128);
        welcome_pt.extend_from_slice(self.server_short_keypair.public.as_bytes()); // s'.pk (32)
        welcome_pt.extend_from_slice(&cookie); // cookie (96)
        // total = 128 bytes

        let welcome_box = salsa_encrypt(
            c_prime_pk.as_bytes(),
            &self.server_keypair.secret.to_raw_bytes(),
            &welcome_nonce_24,
            &welcome_pt,
        )
        .map_err(|_| ZmtpError::Protocol)?;
        // welcome_box = 128 + 16 = 144 bytes

        let mut body = BytesMut::with_capacity(168);
        body.extend_from_slice(CURVE_WELCOME); //   8
        body.extend_from_slice(&server_nonce_16); //  16
        body.extend_from_slice(&welcome_box); // 144
        // total = 168

        write_zmtp_cmd(stream, &body, timeout).await
    }

    /// Receive and fully verify INITIATE command from a ZMTP frame.
    ///
    /// Verifies: cookie integrity, initiate box, vouch box.
    /// Stores the authenticated client long-term public key C and peer metadata.
    #[allow(clippy::similar_names)]
    async fn recv_initiate<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncRead + Unpin,
    {
        debug!("[CURVE SERVER] Waiting for INITIATE");

        let body = read_zmtp_cmd(stream, timeout, MAX_INITIATE_BODY).await?;
        // body = \x08INITIATE(9) + cookie(96) + C(32) + nonce_8(8) + initiate_box(rest)
        // 9 + 96 + 32 + 8 = 145 minimum before initiate_box
        if body.len() < 145 || &body[..9] != CURVE_INITIATE {
            warn!("[CURVE SERVER] Invalid INITIATE frame (len={})", body.len());
            return Err(ZmtpError::Protocol);
        }

        let cookie_bytes = &body[9..105]; // 96 bytes
        let c_pk_buf = &body[105..137]; // 32 bytes C
        let nonce_suffix = &body[137..145]; // 8 bytes
        let initiate_box = &body[145..]; // variable

        // Decrypt cookie to recover (c'.pk ‖ s'.sk_bytes)
        let cookie_nonce_16 = &cookie_bytes[..16];
        let cookie_ct = &cookie_bytes[16..]; // 80 bytes
        let mut cookie_nonce_24 = [0u8; 24];
        cookie_nonce_24[..8].copy_from_slice(b"COOKIE--");
        cookie_nonce_24[8..].copy_from_slice(cookie_nonce_16);

        let cookie_cipher = XChaCha20Poly1305::new(self.cookie_key.as_ref().into());
        let cookie_pt = cookie_cipher
            .decrypt(XNonce::from_slice(&cookie_nonce_24), cookie_ct)
            .map_err(|_| {
                warn!("[CURVE SERVER] Cookie decryption failed (tampered or wrong key)");
                ZmtpError::AuthenticationFailed
            })?;

        if cookie_pt.len() != 64 {
            return Err(ZmtpError::Protocol);
        }
        let mut recovered_c_prime_pk = [0u8; 32];
        recovered_c_prime_pk.copy_from_slice(&cookie_pt[..32]);
        let mut recovered_s_prime_sk = [0u8; 32];
        recovered_s_prime_sk.copy_from_slice(&cookie_pt[32..]);

        // Verify cookie's c'.pk matches the HELLO c'.pk
        let c_prime_pk = self.client_short_public.ok_or(ZmtpError::Protocol)?;
        if recovered_c_prime_pk != *c_prime_pk.as_bytes() {
            warn!("[CURVE SERVER] Cookie c'.pk doesn't match HELLO c'.pk");
            return Err(ZmtpError::AuthenticationFailed);
        }

        let mut c_pk = [0u8; 32];
        c_pk.copy_from_slice(c_pk_buf);

        // Decrypt initiate_box: SalsaBox(c'→s') using s'_sk (from cookie) + c'_pk
        let mut initiate_nonce_24 = [0u8; 24];
        initiate_nonce_24[..16].copy_from_slice(b"CurveZMQINITIATE");
        initiate_nonce_24[16..].copy_from_slice(nonce_suffix);

        let initiate_pt = salsa_decrypt(
            &recovered_c_prime_pk,
            &recovered_s_prime_sk,
            &initiate_nonce_24,
            initiate_box,
        )
        .map_err(|_| {
            warn!("[CURVE SERVER] INITIATE box decryption failed");
            ZmtpError::AuthenticationFailed
        })?;

        // initiate_pt = vouch(96) + metadata(0+)
        if initiate_pt.len() < 96 {
            warn!(
                "[CURVE SERVER] INITIATE plaintext too short: {}",
                initiate_pt.len()
            );
            return Err(ZmtpError::Protocol);
        }

        // Extract and verify vouch: vouch_nonce_16(16) + vouch_ct(80) = 96 bytes
        let vouch_nonce_16 = &initiate_pt[..16];
        let vouch_ct = &initiate_pt[16..96];
        let mut vouch_nonce_24 = [0u8; 24];
        vouch_nonce_24[..8].copy_from_slice(b"VOUCH---");
        vouch_nonce_24[8..].copy_from_slice(vouch_nonce_16);

        // Decrypt vouch: SalsaBox(C→S) using S_sk + C_pk
        let vouch_pt = salsa_decrypt(
            &c_pk,
            &self.server_keypair.secret.to_raw_bytes(),
            &vouch_nonce_24,
            vouch_ct,
        )
        .map_err(|_| {
            warn!("[CURVE SERVER] Vouch verification failed");
            ZmtpError::AuthenticationFailed
        })?;

        // vouch plaintext must be c'.pk(32) + s'.pk(32)
        if vouch_pt.len() != 64 {
            warn!(
                "[CURVE SERVER] Vouch plaintext wrong size: {}",
                vouch_pt.len()
            );
            return Err(ZmtpError::AuthenticationFailed);
        }
        if &vouch_pt[..32] != c_prime_pk.as_bytes() {
            warn!("[CURVE SERVER] Vouch c'.pk mismatch");
            return Err(ZmtpError::AuthenticationFailed);
        }
        if &vouch_pt[32..] != self.server_short_keypair.public.as_bytes() {
            warn!("[CURVE SERVER] Vouch s'.pk mismatch");
            return Err(ZmtpError::AuthenticationFailed);
        }

        // Parse metadata from initiate_pt[96..]
        let metadata = &initiate_pt[96..];
        let (peer_st, peer_id) = decode_zmtp_props(metadata)?;
        if peer_st.is_none() {
            warn!("[CURVE SERVER] INITIATE missing Socket-Type in metadata");
            return Err(ZmtpError::Protocol);
        }
        self.peer_socket_type = peer_st;
        self.peer_identity_recv = peer_id;

        self.client_public = Some(CurvePublicKey::from_bytes(c_pk));
        debug!("[CURVE SERVER] INITIATE verified - client authenticated");
        Ok(())
    }

    /// Send READY command wrapped in a ZMTP command frame.
    /// Encrypts server metadata in a READY box, derives message key.
    async fn send_ready<S>(
        &mut self,
        stream: &mut S,
        timeout: Option<Duration>,
    ) -> Result<(), ZmtpError>
    where
        S: AsyncWrite + Unpin,
    {
        debug!("[CURVE SERVER] Sending READY");

        let c_prime = self.client_short_public.ok_or(ZmtpError::Protocol)?;
        let c = self.client_public.ok_or(ZmtpError::Protocol)?;

        // Derive message key: SHA-256(c'·S ‖ C·s' ‖ c'·s')
        let dh1 = self.server_keypair.secret.diffie_hellman(&c_prime); // S·c' = c'·S
        let dh2 = self.server_short_keypair.secret.diffie_hellman(&c); // s'·C = C·s'
        let dh3 = self.server_short_keypair.secret.diffie_hellman(&c_prime); // s'·c' = c'·s'
        let key = derive_message_key(&dh1, &dh2, &dh3);
        self.message_box = Some(CurveBox::new(&key));

        // Encrypt server metadata in READY box (s'→c')
        let ready_counter: u64 = 1;
        let mut ready_nonce_24 = [0u8; 24];
        ready_nonce_24[..16].copy_from_slice(CURVE_READY_NONCE_PREFIX);
        ready_nonce_24[16..].copy_from_slice(&ready_counter.to_be_bytes());

        let server_meta = encode_zmtp_props(&self.local_socket_type, None);
        let ready_box = salsa_encrypt(
            c_prime.as_bytes(),
            &self.server_short_keypair.secret.to_raw_bytes(),
            &ready_nonce_24,
            &server_meta,
        )
        .map_err(|_| ZmtpError::Protocol)?;

        let mut body = Vec::with_capacity(6 + 8 + ready_box.len());
        body.extend_from_slice(CURVE_READY);
        body.extend_from_slice(&ready_counter.to_be_bytes());
        body.extend_from_slice(&ready_box);

        write_zmtp_cmd(stream, &body, timeout).await?;
        debug!("[CURVE SERVER] Handshake complete");
        Ok(())
    }

    /// Encrypt a message for the client
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Bytes, CurveError> {
        let message_box = self
            .message_box
            .as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        // Nonce = "CurveZMQMESSAGES" + 8-byte counter (server→client)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGES");
        nonce[16..].copy_from_slice(&self.send_nonce.to_be_bytes());
        self.send_nonce = self
            .send_nonce
            .checked_add(1)
            .ok_or(CurveError::ProtocolViolation)?;

        let ciphertext = message_box.encrypt(plaintext, &nonce)?;

        let mut message = BytesMut::with_capacity(CURVE_MESSAGE.len() + 8 + ciphertext.len());
        message.extend_from_slice(CURVE_MESSAGE);
        message.extend_from_slice(&nonce[16..]); // 8-byte suffix only
        message.extend_from_slice(&ciphertext);

        Ok(message.freeze())
    }

    /// Decrypt a message from the client
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Bytes, CurveError> {
        let parts = parse_curve_message(message)?;
        let message_box = self
            .message_box
            .as_ref()
            .ok_or(CurveError::ProtocolViolation)?;

        let counter = u64::from_be_bytes(
            parts
                .short_nonce
                .try_into()
                .map_err(|_| CurveError::InvalidNonce)?,
        );
        if counter < self.recv_nonce {
            return Err(CurveError::ProtocolViolation); // replay
        }
        self.recv_nonce = counter
            .checked_add(1)
            .ok_or(CurveError::ProtocolViolation)?;

        // Reconstruct full 24-byte nonce (client→server direction)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGEC");
        nonce[16..].copy_from_slice(parts.short_nonce);

        let plaintext = message_box.decrypt(parts.ciphertext, &nonce)?;
        Ok(Bytes::from(plaintext))
    }
}

// ── ZAP helpers ───────────────────────────────────────────────────────────────

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
/// Performs the full CURVE handshake (HELLO/WELCOME/INITIATE/READY) and then
/// authenticates the client's long-term public key via ZAP.  On ZAP failure a
/// ZMTP ERROR command is sent before closing.
pub async fn curve_server_handshake_zap<S>(
    stream: &mut S,
    server_keypair: CurveKeyPair,
    domain: String,
    timeout: Option<Duration>,
    peer_addr: &str,
    local_socket_type: impl Into<String>,
) -> Result<CurveHandshakeResult, ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use crate::security::zap_client::ZapClient;

    debug!("[CURVE SERVER ZAP] Starting ZAP-authenticated handshake");

    let mut curve_server = CurveServer::new(server_keypair, local_socket_type);
    let result = curve_server.handshake(stream, timeout).await?;

    let client_public_key = result.peer_public_key.ok_or(ZmtpError::Protocol)?;

    debug!("[CURVE SERVER ZAP] Handshake complete, authenticating via ZAP");

    let zap_timeout = timeout.unwrap_or(Duration::from_secs(5));
    let mut zap_client = ZapClient::new(zap_timeout).map_err(|e| {
        warn!("[CURVE SERVER ZAP] Failed to create ZAP client: {}", e);
        ZmtpError::AuthenticationFailed
    })?;

    let zap_response = zap_client
        .authenticate_curve(client_public_key.as_bytes(), &domain, peer_addr)
        .await
        .map_err(|e| {
            warn!("[CURVE SERVER ZAP] ZAP request failed: {}", e);
            ZmtpError::AuthenticationFailed
        })?;

    if matches!(zap_response.status_code, ZapStatus::Success) {
        debug!(
            "[CURVE SERVER ZAP] Authentication successful for client key: {:?}",
            client_public_key
        );
        Ok(result)
    } else {
        warn!(
            "[CURVE SERVER ZAP] Authentication failed for {}: {} (status: {:?})",
            peer_addr, zap_response.status_text, zap_response.status_code
        );
        send_zmtp_error(stream, &zap_response.status_text).await;
        Err(ZmtpError::AuthenticationFailed)
    }
}

/// Send a ZMTP ERROR command frame to the peer (best-effort).
async fn send_zmtp_error<S>(stream: &mut S, reason: &str)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use compio_buf::BufResult;

    // Cap reason at 248 bytes: body = 6 ("\x05ERROR") + 1 (len byte) + reason ≤ 255 (short-frame limit)
    let reason_bytes = reason.as_bytes();
    let reason_len = reason_bytes.len().min(248) as u8;

    let mut body = BytesMut::with_capacity(7 + reason_len as usize);
    body.extend_from_slice(b"\x05ERROR");
    body.extend_from_slice(&[reason_len]);
    body.extend_from_slice(&reason_bytes[..reason_len as usize]);
    // body.len() ≤ 255, so the short ZMTP frame format (0x04 + 1-byte length) is always valid.

    let body_len = body.len() as u8;
    let mut frame = BytesMut::with_capacity(2 + body_len as usize);
    frame.extend_from_slice(&[0x04, body_len]);
    frame.extend_from_slice(&body);

    let BufResult(_, _) = stream.write_all(frame.freeze()).await;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use compio_buf::{BufResult, IoBuf, IoBufMut};
    use std::collections::VecDeque;
    use std::io;

    const ERROR_REASON: &str = "denied";

    #[derive(Debug)]
    struct PartialWriteStream {
        write_limits: VecDeque<usize>,
        written: Vec<u8>,
    }

    impl PartialWriteStream {
        fn new(write_limits: impl IntoIterator<Item = usize>) -> Self {
            Self {
                write_limits: write_limits.into_iter().collect(),
                written: Vec::new(),
            }
        }

        fn written(&self) -> &[u8] {
            &self.written
        }
    }

    impl AsyncRead for PartialWriteStream {
        async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
            BufResult(Ok(0), buf)
        }
    }

    impl AsyncWrite for PartialWriteStream {
        async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
            let len = self
                .write_limits
                .pop_front()
                .unwrap_or_else(|| buf.buf_len())
                .min(buf.buf_len());
            self.written.extend_from_slice(&buf.as_slice()[..len]);
            BufResult(Ok(len), buf)
        }

        async fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn expected_zmtp_error_frame(reason: &str) -> Vec<u8> {
        let reason = reason.as_bytes();
        let reason_len = reason.len().min(248);
        let mut frame = Vec::with_capacity(2 + 7 + reason_len);
        frame.push(0x04);
        frame.push((7 + reason_len) as u8);
        frame.extend_from_slice(b"\x05ERROR");
        frame.push(reason_len as u8);
        frame.extend_from_slice(&reason[..reason_len]);
        frame
    }

    async fn assert_zmtp_error_survives_partial_writes(
        write_limits: impl IntoIterator<Item = usize>,
    ) {
        let mut stream = PartialWriteStream::new(write_limits);
        send_zmtp_error(&mut stream, ERROR_REASON).await;
        assert_eq!(stream.written(), expected_zmtp_error_frame(ERROR_REASON));
    }

    #[test]
    fn test_keypair_generation() {
        let keypair = CurveKeyPair::generate();
        assert_eq!(keypair.public.as_bytes().len(), CURVE_KEY_SIZE);
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
    fn diffie_hellman_rejects_non_contributory_peer_public_key() {
        let secret = CurveSecretKey::generate();
        let low_order_public = CurvePublicKey::from_bytes([0u8; CURVE_KEY_SIZE]);

        let shared = secret.diffie_hellman(&low_order_public);

        assert_ne!(
            shared, [0u8; CURVE_KEY_SIZE],
            "CURVE accepted a non-contributory X25519 peer key and produced an all-zero shared secret"
        );
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
    fn test_salsa_box_round_trip() {
        let alice = CurveKeyPair::generate();
        let bob = CurveKeyPair::generate();

        let nonce = [7u8; 24];
        let pt = b"test message";

        let ct = salsa_encrypt(
            bob.public.as_bytes(),
            &alice.secret.to_raw_bytes(),
            &nonce,
            pt,
        )
        .unwrap();
        let recovered = salsa_decrypt(
            alice.public.as_bytes(),
            &bob.secret.to_raw_bytes(),
            &nonce,
            &ct,
        )
        .unwrap();

        assert_eq!(recovered, pt);
    }

    #[test]
    #[allow(clippy::similar_names)]
    fn test_3way_dh_message_key_symmetry() {
        let client_long = CurveKeyPair::generate();
        let server_long = CurveKeyPair::generate();
        let client_short = CurveKeyPair::generate();
        let server_short = CurveKeyPair::generate();

        // Client side
        let dh1c = client_short.secret.diffie_hellman(&server_long.public);
        let dh2c = client_long.secret.diffie_hellman(&server_short.public);
        let dh3c = client_short.secret.diffie_hellman(&server_short.public);
        let client_key = derive_message_key(&dh1c, &dh2c, &dh3c);

        // Server side
        let dh1s = server_long.secret.diffie_hellman(&client_short.public);
        let dh2s = server_short.secret.diffie_hellman(&client_long.public);
        let dh3s = server_short.secret.diffie_hellman(&client_short.public);
        let server_key = derive_message_key(&dh1s, &dh2s, &dh3s);

        assert_eq!(
            client_key, server_key,
            "both sides must derive the same message key"
        );
    }

    #[test]
    fn client_decrypt_message_accepts_valid_curve_message_command_header() {
        let shared_secret = [42u8; CURVE_KEY_SIZE];
        let box_ = CurveBox::new(&shared_secret);

        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGES");
        nonce[16..].copy_from_slice(&1u64.to_be_bytes());

        let ciphertext = box_.encrypt(b"server message", &nonce).unwrap();
        let mut frame = BytesMut::new();
        frame.extend_from_slice(CURVE_MESSAGE);
        frame.extend_from_slice(&nonce[16..]);
        frame.extend_from_slice(&ciphertext);

        let client_keypair = CurveKeyPair::generate();
        let server_public = CurveKeyPair::generate().public;
        let mut client = CurveClient::new(client_keypair, server_public, "DEALER", None);
        client.message_box = Some(CurveBox::new(&shared_secret));

        let plaintext = client.decrypt_message(&frame).unwrap();
        assert_eq!(plaintext.as_ref(), b"server message");
    }

    #[test]
    fn server_decrypt_message_accepts_valid_curve_message_command_header() {
        let shared_secret = [43u8; CURVE_KEY_SIZE];
        let box_ = CurveBox::new(&shared_secret);

        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGEC");
        nonce[16..].copy_from_slice(&2u64.to_be_bytes());

        let ciphertext = box_.encrypt(b"client message", &nonce).unwrap();
        let mut frame = BytesMut::new();
        frame.extend_from_slice(CURVE_MESSAGE);
        frame.extend_from_slice(&nonce[16..]);
        frame.extend_from_slice(&ciphertext);

        let server_keypair = CurveKeyPair::generate();
        let mut server = CurveServer::new(server_keypair, "ROUTER");
        server.message_box = Some(CurveBox::new(&shared_secret));

        let plaintext = server.decrypt_message(&frame).unwrap();
        assert_eq!(plaintext.as_ref(), b"client message");
    }

    #[test]
    fn decrypt_message_rejects_invalid_curve_message_command_header() {
        let client_keypair = CurveKeyPair::generate();
        let server_public = CurveKeyPair::generate().public;
        let mut client = CurveClient::new(client_keypair, server_public, "DEALER", None);
        client.message_box = Some(CurveBox::new(&[42u8; CURVE_KEY_SIZE]));

        let mut frame = BytesMut::new();
        frame.extend_from_slice(b"\x05READY");
        frame.extend_from_slice(&1u64.to_be_bytes());
        frame.extend_from_slice(&[0u8; CURVE_BOX_OVERHEAD]);

        assert!(matches!(
            client.decrypt_message(&frame),
            Err(CurveError::ProtocolViolation)
        ));
    }

    #[test]
    fn decrypt_message_rejects_missing_curve_message_nonce() {
        let client_keypair = CurveKeyPair::generate();
        let server_public = CurveKeyPair::generate().public;
        let mut client = CurveClient::new(client_keypair, server_public, "DEALER", None);
        client.message_box = Some(CurveBox::new(&[42u8; CURVE_KEY_SIZE]));

        assert!(matches!(
            client.decrypt_message(CURVE_MESSAGE),
            Err(CurveError::ProtocolViolation)
        ));
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

    #[test]
    fn test_send_zmtp_error_retries_short_writes() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_send_zmtp_error_retries_short_writes_impl())
    }

    async fn test_send_zmtp_error_retries_short_writes_impl() {
        assert_zmtp_error_survives_partial_writes([2, 3]).await;
    }

    #[test]
    fn test_send_zmtp_error_retries_short_body_writes() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_send_zmtp_error_retries_short_body_writes_impl())
    }

    async fn test_send_zmtp_error_retries_short_body_writes_impl() {
        assert_zmtp_error_survives_partial_writes([9, 2]).await;
    }

    #[test]
    fn test_encode_decode_zmtp_props_round_trip() {
        let encoded = encode_zmtp_props("DEALER", Some(b"my-identity"));
        let (st, id) = decode_zmtp_props(&encoded).unwrap();
        assert_eq!(st.unwrap().as_ref(), b"DEALER");
        assert_eq!(id.unwrap().as_ref(), b"my-identity");
    }

    #[test]
    fn test_encode_decode_zmtp_props_no_identity() {
        let encoded = encode_zmtp_props("ROUTER", None);
        let (st, id) = decode_zmtp_props(&encoded).unwrap();
        assert_eq!(st.unwrap().as_ref(), b"ROUTER");
        assert!(id.is_none());
    }

    #[test]
    fn decode_zmtp_props_rejects_duplicate_socket_type_property() {
        let mut encoded = Vec::new();
        push_prop(&mut encoded, b"Socket-Type", b"DEALER");
        push_prop(&mut encoded, b"Socket-Type", b"ROUTER");

        assert!(matches!(
            decode_zmtp_props(&encoded),
            Err(ZmtpError::Protocol)
        ));
    }

    #[test]
    fn decode_zmtp_props_rejects_duplicate_identity_property() {
        let mut encoded = Vec::new();
        push_prop(&mut encoded, b"Socket-Type", b"DEALER");
        push_prop(&mut encoded, b"Identity", b"trusted");
        push_prop(&mut encoded, b"Identity", b"shadow");

        assert!(matches!(
            decode_zmtp_props(&encoded),
            Err(ZmtpError::Protocol)
        ));
    }
}
