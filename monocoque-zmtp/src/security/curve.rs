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
//!   |<-- READY (confirmation) ------------|
//!   |                                      |
//!   |<=== Encrypted MESSAGE frames ======>|
//! ```
//!
//! ## Wire Sizes
//!
//! - HELLO:    198 bytes = 6 + 1 + 71 + 32 + 8 + 80
//! - WELCOME:  168 bytes = 8 + 16 + 144
//! - INITIATE: 257 bytes = 9 + 96 + 32 + 8 + 112  (empty metadata)
//! - READY:      6 bytes (header only)
//!
//! ## References
//!
//! - RFC 26: <https://rfc.zeromq.org/spec/26/>

use bytes::{Bytes, BytesMut};
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use compio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use crypto_box::{
    aead::generic_array::GenericArray,
    PublicKey as SalsaPublicKey, SalsaBox, SecretKey as SalsaSecretKey,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
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
const CURVE_MESSAGE_NONCE_SIZE: usize = 8;

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
        *self.0.diffie_hellman(&peer_public.to_x25519()).as_bytes()
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
struct CurveBox {
    cipher: XChaCha20Poly1305,
}

impl CurveBox {
    fn new(key: &[u8; CURVE_KEY_SIZE]) -> Self {
        let cipher = XChaCha20Poly1305::new(key.into());
        Self { cipher }
    }

    fn encrypt(&self, plaintext: &[u8], nonce: &[u8; CURVE_NONCE_SIZE]) -> Result<Vec<u8>, CurveError> {
        let nonce = XNonce::from_slice(nonce);
        self.cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| CurveError::EncryptionFailed)
    }

    fn decrypt(&self, ciphertext: &[u8], nonce: &[u8; CURVE_NONCE_SIZE]) -> Result<Vec<u8>, CurveError> {
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

#[inline(always)]
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
    /// Send nonce counter
    send_nonce: u64,
    /// Next expected receive nonce counter (replay protection)
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
            cookie: None,
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

    /// Send HELLO command (198 bytes)
    ///
    /// Body: version(1) + padding(71) + c'.pk(32) + nonce_suffix(8) + hello_box(80)
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
        use compio::buf::BufResult;
        use monocoque_core::timeout::write_all_with_timeout;

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
        frame.extend_from_slice(CURVE_HELLO);                                     //  6
        frame.extend_from_slice(&[1u8]);                                          //  1  version
        frame.extend_from_slice(&[0u8; 71]);                                      // 71  padding
        frame.extend_from_slice(self.client_short_keypair.public.as_bytes());     // 32  c'.pk
        frame.extend_from_slice(&nonce_counter.to_be_bytes());                    //  8  nonce
        frame.extend_from_slice(&hello_box);                                      // 80  box
        // total = 198

        let BufResult(r, _) =
            write_all_with_timeout(stream, frame.freeze().to_vec(), timeout)
                .await
                .map_err(ZmtpError::from)?;
        r.map_err(Into::into)
    }

    /// Receive and decrypt WELCOME command (168 bytes)
    ///
    /// Body: server_nonce(16) + welcome_box(144)
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
        use compio::buf::BufResult;
        use monocoque_core::timeout::read_exact_with_timeout;

        debug!("[CURVE CLIENT] Waiting for WELCOME");

        let header = vec![0u8; CURVE_WELCOME.len()];
        let BufResult(r, header) =
            read_exact_with_timeout(stream, header, timeout).await.map_err(ZmtpError::from)?;
        r?;
        if &header[..] != CURVE_WELCOME {
            warn!("[CURVE CLIENT] Invalid WELCOME header");
            return Err(ZmtpError::Protocol);
        }

        // server_nonce: 16 random bytes
        let BufResult(r, server_nonce_16) =
            read_exact_with_timeout(stream, vec![0u8; 16], timeout).await.map_err(ZmtpError::from)?;
        r?;

        // welcome_box: 144 bytes
        let BufResult(r, welcome_box) =
            read_exact_with_timeout(stream, vec![0u8; 144], timeout).await.map_err(ZmtpError::from)?;
        r?;

        // Decrypt: SalsaBox(S→c') using c'_sk + S_pk
        let mut nonce_24 = [0u8; 24];
        nonce_24[..8].copy_from_slice(b"WELCOME-");
        nonce_24[8..].copy_from_slice(&server_nonce_16);

        let plaintext = salsa_decrypt(
            self.server_public.as_bytes(),
            &self.client_short_keypair.secret.to_raw_bytes(),
            &nonce_24,
            &welcome_box,
        )
        .map_err(|_| {
            warn!("[CURVE CLIENT] Failed to decrypt WELCOME box");
            ZmtpError::Protocol
        })?;

        // plaintext = s'.pk(32) + cookie(96) = 128 bytes
        if plaintext.len() != 128 {
            warn!("[CURVE CLIENT] WELCOME plaintext wrong size: {}", plaintext.len());
            return Err(ZmtpError::Protocol);
        }

        let mut s_prime_pk = [0u8; 32];
        s_prime_pk.copy_from_slice(&plaintext[..32]);
        self.server_short_public = Some(CurvePublicKey::from_bytes(s_prime_pk));
        self.cookie = Some(plaintext[32..128].to_vec());

        debug!("[CURVE CLIENT] Received WELCOME, stored s'.pk and cookie");
        Ok(())
    }

    /// Send INITIATE command (257 bytes with empty metadata)
    ///
    /// Body: cookie(96) + C(32) + nonce_suffix(8) + initiate_box(112)
    ///
    /// initiate_box = SalsaBox(c'→s').encrypt(vouch, "CurveZMQINITIATE" ‖ nonce_suffix)
    /// vouch = vouch_nonce_16(16) + SalsaBox(C→S).encrypt(c'.pk ‖ s'.pk, "VOUCH---" ‖ vouch_nonce_16)
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
        vouch_pt[32..].copy_from_slice(server_short_public.as_bytes());              // s'.pk

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

        // Build initiate_box: Box[vouch ‖ metadata](c'→s')
        let initiate_counter: u64 = 1;
        let mut initiate_nonce_24 = [0u8; 24];
        initiate_nonce_24[..16].copy_from_slice(b"CurveZMQINITIATE");
        initiate_nonce_24[16..].copy_from_slice(&initiate_counter.to_be_bytes());

        let initiate_box = salsa_encrypt(
            server_short_public.as_bytes(),
            &self.client_short_keypair.secret.to_raw_bytes(),
            &initiate_nonce_24,
            &vouch, // plaintext = vouch (96), no metadata
        )
        .map_err(|_| ZmtpError::Protocol)?;
        // initiate_box = 96 + 16 = 112 bytes

        let mut frame = BytesMut::with_capacity(257);
        frame.extend_from_slice(CURVE_INITIATE);                               //   9
        frame.extend_from_slice(cookie);                                        //  96
        frame.extend_from_slice(self.client_keypair.public.as_bytes());        //  32  C
        frame.extend_from_slice(&initiate_counter.to_be_bytes());              //   8
        frame.extend_from_slice(&initiate_box);                                // 112
        // total = 257

        let BufResult(r, _) =
            write_all_with_timeout(stream, frame.freeze().to_vec(), timeout)
                .await
                .map_err(ZmtpError::from)?;
        r.map_err(Into::into)
    }

    /// Receive READY command and derive the message box key
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

        let BufResult(r, header) =
            read_exact_with_timeout(stream, vec![0u8; CURVE_READY.len()], timeout)
                .await
                .map_err(ZmtpError::from)?;
        r?;

        if &header[..] != CURVE_READY {
            warn!("[CURVE CLIENT] Invalid READY header");
            return Err(ZmtpError::Protocol);
        }

        // Derive message key: SHA-256(c'·S ‖ C·s' ‖ c'·s')
        let s_prime = self.server_short_public.ok_or(ZmtpError::Protocol)?;
        let dh1 = self.client_short_keypair.secret.diffie_hellman(&self.server_public); // c'·S
        let dh2 = self.client_keypair.secret.diffie_hellman(&s_prime);                  // C·s'
        let dh3 = self.client_short_keypair.secret.diffie_hellman(&s_prime);            // c'·s'

        let key = derive_message_key(&dh1, &dh2, &dh3);
        self.message_box = Some(CurveBox::new(&key));

        debug!("[CURVE CLIENT] Handshake complete");
        Ok(())
    }

    /// Encrypt a message for the server
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Bytes, CurveError> {
        let message_box = self.message_box.as_ref().ok_or(CurveError::ProtocolViolation)?;

        // Nonce = "CurveZMQMESSAGEC" + 8-byte counter (client→server)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGEC");
        nonce[16..].copy_from_slice(&self.send_nonce.to_be_bytes());
        self.send_nonce += 1;

        let ciphertext = message_box.encrypt(plaintext, &nonce)?;

        let mut message = BytesMut::new();
        message.extend_from_slice(CURVE_MESSAGE);
        message.extend_from_slice(&nonce[16..]); // 8-byte counter suffix only
        message.extend_from_slice(&ciphertext);

        Ok(message.freeze())
    }

    /// Decrypt a message from the server
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Bytes, CurveError> {
        let parts = parse_curve_message(message)?;
        let message_box = self.message_box.as_ref().ok_or(CurveError::ProtocolViolation)?;

        let counter = u64::from_be_bytes(
            parts.short_nonce.try_into().map_err(|_| CurveError::InvalidNonce)?,
        );
        if counter < self.recv_nonce {
            return Err(CurveError::ProtocolViolation); // replay
        }
        self.recv_nonce = counter + 1;

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
    /// Send nonce counter
    send_nonce: u64,
    /// Next expected receive nonce counter (replay protection)
    recv_nonce: u64,
    /// Encryption box for messages (after READY)
    message_box: Option<CurveBox>,
}

impl CurveServer {
    /// Create new CURVE server
    pub fn new(server_keypair: CurveKeyPair) -> Self {
        let mut cookie_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut cookie_key);
        Self {
            server_keypair,
            server_short_keypair: CurveKeyPair::generate(),
            client_short_public: None,
            client_public: None,
            cookie_key,
            send_nonce: 1,
            recv_nonce: 1,
            message_box: None,
        }
    }

    /// Perform full server handshake: HELLO → WELCOME → INITIATE → READY
    ///
    /// Returns the client's authenticated long-term public key.
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
        Ok(self.client_public.unwrap())
    }

    /// Receive and verify HELLO command
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

        let BufResult(r, header) =
            read_exact_with_timeout(stream, vec![0u8; CURVE_HELLO.len()], timeout)
                .await
                .map_err(ZmtpError::from)?;
        r?;
        if &header[..] != CURVE_HELLO {
            warn!("[CURVE SERVER] Invalid HELLO header");
            return Err(ZmtpError::Protocol);
        }

        // version (1 byte)
        let BufResult(r, ver) =
            read_exact_with_timeout(stream, vec![0u8; 1], timeout).await.map_err(ZmtpError::from)?;
        r?;
        if ver[0] != 1 {
            warn!("[CURVE SERVER] Unsupported HELLO version: {}", ver[0]);
            return Err(ZmtpError::Protocol);
        }

        // padding (71 bytes) — discard
        let BufResult(r, _) =
            read_exact_with_timeout(stream, vec![0u8; 71], timeout).await.map_err(ZmtpError::from)?;
        r?;

        // c'.pk (32 bytes)
        let BufResult(r, c_prime_pk_buf) =
            read_exact_with_timeout(stream, vec![0u8; 32], timeout).await.map_err(ZmtpError::from)?;
        r?;
        let mut c_prime_pk = [0u8; 32];
        c_prime_pk.copy_from_slice(&c_prime_pk_buf);
        self.client_short_public = Some(CurvePublicKey::from_bytes(c_prime_pk));

        // nonce_suffix (8 bytes) — RFC 26 §5.2 requires the counter to be >= 1
        let BufResult(r, nonce_suffix) =
            read_exact_with_timeout(stream, vec![0u8; 8], timeout).await.map_err(ZmtpError::from)?;
        r?;
        if u64::from_be_bytes(nonce_suffix.as_slice().try_into().unwrap_or([0u8; 8])) == 0 {
            warn!("[CURVE SERVER] HELLO nonce counter is 0, must be >= 1");
            return Err(ZmtpError::Protocol);
        }

        // hello_box (80 bytes)
        let BufResult(r, hello_box) =
            read_exact_with_timeout(stream, vec![0u8; 80], timeout).await.map_err(ZmtpError::from)?;
        r?;

        // Verify: SalsaBox(S→c') decrypt using S_sk + c'_pk
        let mut nonce_24 = [0u8; 24];
        nonce_24[..16].copy_from_slice(b"CurveZMQHELLO---");
        nonce_24[16..].copy_from_slice(&nonce_suffix);

        let plaintext = salsa_decrypt(
            &c_prime_pk,
            &self.server_keypair.secret.to_raw_bytes(),
            &nonce_24,
            &hello_box,
        )
        .map_err(|_| {
            warn!("[CURVE SERVER] HELLO box authentication failed");
            ZmtpError::AuthenticationFailed
        })?;

        // The box plaintext must be exactly 64 zero bytes
        if plaintext.len() != 64 || plaintext.iter().any(|&b| b != 0) {
            warn!("[CURVE SERVER] HELLO box plaintext is not 64 zeros");
            return Err(ZmtpError::AuthenticationFailed);
        }

        debug!("[CURVE SERVER] HELLO verified");
        Ok(())
    }

    /// Build and send WELCOME command (168 bytes)
    ///
    /// Body: server_nonce_16(16) + welcome_box(144)
    ///
    /// Cookie = cookie_nonce_16(16) + XChaCha20.encrypt(c'.pk ‖ s'.sk, "COOKIE--" ‖ cookie_nonce_16)
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
        cookie.extend_from_slice(&cookie_ct);        // 80
        // cookie = 96 bytes

        // Build welcome_box (144 bytes): SalsaBox(S→c').encrypt(s'.pk ‖ cookie)
        let mut server_nonce_16 = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut server_nonce_16);
        let mut welcome_nonce_24 = [0u8; 24];
        welcome_nonce_24[..8].copy_from_slice(b"WELCOME-");
        welcome_nonce_24[8..].copy_from_slice(&server_nonce_16);

        let mut welcome_pt = Vec::with_capacity(128);
        welcome_pt.extend_from_slice(self.server_short_keypair.public.as_bytes()); // s'.pk (32)
        welcome_pt.extend_from_slice(&cookie);                                      // cookie (96)
        // total = 128 bytes

        let welcome_box = salsa_encrypt(
            c_prime_pk.as_bytes(),
            &self.server_keypair.secret.to_raw_bytes(),
            &welcome_nonce_24,
            &welcome_pt,
        )
        .map_err(|_| ZmtpError::Protocol)?;
        // welcome_box = 128 + 16 = 144 bytes

        let mut frame = BytesMut::with_capacity(168);
        frame.extend_from_slice(CURVE_WELCOME);    //   8
        frame.extend_from_slice(&server_nonce_16); //  16
        frame.extend_from_slice(&welcome_box);     // 144
        // total = 168

        let BufResult(r, _) =
            write_all_with_timeout(stream, frame.freeze().to_vec(), timeout)
                .await
                .map_err(ZmtpError::from)?;
        r.map_err(Into::into)
    }

    /// Receive and fully verify INITIATE command
    ///
    /// Verifies: cookie integrity, initiate box, vouch box.
    /// Stores the authenticated client long-term public key C.
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

        let BufResult(r, header) =
            read_exact_with_timeout(stream, vec![0u8; CURVE_INITIATE.len()], timeout)
                .await
                .map_err(ZmtpError::from)?;
        r?;
        if &header[..] != CURVE_INITIATE {
            warn!("[CURVE SERVER] Invalid INITIATE header");
            return Err(ZmtpError::Protocol);
        }

        // cookie (96 bytes)
        let BufResult(r, cookie_bytes) =
            read_exact_with_timeout(stream, vec![0u8; 96], timeout).await.map_err(ZmtpError::from)?;
        r?;

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

        // C (client long-term pk, 32 bytes, cleartext)
        let BufResult(r, c_pk_buf) =
            read_exact_with_timeout(stream, vec![0u8; 32], timeout).await.map_err(ZmtpError::from)?;
        r?;
        let mut c_pk = [0u8; 32];
        c_pk.copy_from_slice(&c_pk_buf);

        // nonce_suffix (8 bytes)
        let BufResult(r, nonce_suffix) =
            read_exact_with_timeout(stream, vec![0u8; 8], timeout).await.map_err(ZmtpError::from)?;
        r?;

        // initiate_box (112 bytes for empty metadata)
        let BufResult(r, initiate_box) =
            read_exact_with_timeout(stream, vec![0u8; 112], timeout).await.map_err(ZmtpError::from)?;
        r?;

        // Decrypt initiate_box: SalsaBox(c'→s') using s'_sk (from cookie) + c'_pk
        let mut initiate_nonce_24 = [0u8; 24];
        initiate_nonce_24[..16].copy_from_slice(b"CurveZMQINITIATE");
        initiate_nonce_24[16..].copy_from_slice(&nonce_suffix);

        let initiate_pt = salsa_decrypt(
            &recovered_c_prime_pk,
            &recovered_s_prime_sk,
            &initiate_nonce_24,
            &initiate_box,
        )
        .map_err(|_| {
            warn!("[CURVE SERVER] INITIATE box decryption failed");
            ZmtpError::AuthenticationFailed
        })?;

        // initiate_pt = vouch(96) + metadata(0+)
        if initiate_pt.len() < 96 {
            warn!("[CURVE SERVER] INITIATE plaintext too short: {}", initiate_pt.len());
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
            warn!("[CURVE SERVER] Vouch plaintext wrong size: {}", vouch_pt.len());
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

        self.client_public = Some(CurvePublicKey::from_bytes(c_pk));
        debug!("[CURVE SERVER] INITIATE verified — client authenticated");
        Ok(())
    }

    /// Send READY command and derive the message box key
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

        let BufResult(r, _) =
            write_all_with_timeout(stream, Bytes::from_static(CURVE_READY).to_vec(), timeout)
                .await
                .map_err(ZmtpError::from)?;
        r?;

        // Derive message key: SHA-256(c'·S ‖ C·s' ‖ c'·s')
        let c_prime = self.client_short_public.ok_or(ZmtpError::Protocol)?;
        let c = self.client_public.ok_or(ZmtpError::Protocol)?;
        let dh1 = self.server_keypair.secret.diffie_hellman(&c_prime);       // S·c' = c'·S
        let dh2 = self.server_short_keypair.secret.diffie_hellman(&c);       // s'·C = C·s'
        let dh3 = self.server_short_keypair.secret.diffie_hellman(&c_prime); // s'·c' = c'·s'

        let key = derive_message_key(&dh1, &dh2, &dh3);
        self.message_box = Some(CurveBox::new(&key));

        debug!("[CURVE SERVER] Handshake complete");
        Ok(())
    }

    /// Encrypt a message for the client
    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<Bytes, CurveError> {
        let message_box = self.message_box.as_ref().ok_or(CurveError::ProtocolViolation)?;

        // Nonce = "CurveZMQMESSAGES" + 8-byte counter (server→client)
        let mut nonce = [0u8; CURVE_NONCE_SIZE];
        nonce[..16].copy_from_slice(b"CurveZMQMESSAGES");
        nonce[16..].copy_from_slice(&self.send_nonce.to_be_bytes());
        self.send_nonce += 1;

        let ciphertext = message_box.encrypt(plaintext, &nonce)?;

        let mut message = BytesMut::new();
        message.extend_from_slice(CURVE_MESSAGE);
        message.extend_from_slice(&nonce[16..]); // 8-byte suffix only
        message.extend_from_slice(&ciphertext);

        Ok(message.freeze())
    }

    /// Decrypt a message from the client
    pub fn decrypt_message(&mut self, message: &[u8]) -> Result<Bytes, CurveError> {
        let parts = parse_curve_message(message)?;
        let message_box = self.message_box.as_ref().ok_or(CurveError::ProtocolViolation)?;

        let counter = u64::from_be_bytes(
            parts.short_nonce.try_into().map_err(|_| CurveError::InvalidNonce)?,
        );
        if counter < self.recv_nonce {
            return Err(CurveError::ProtocolViolation); // replay
        }
        self.recv_nonce = counter + 1;

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
) -> Result<CurvePublicKey, ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use crate::security::zap_client::ZapClient;

    debug!("[CURVE SERVER ZAP] Starting ZAP-authenticated handshake");

    let mut curve_server = CurveServer::new(server_keypair);
    let client_public_key = curve_server.handshake(stream, timeout).await?;

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
        Ok(client_public_key)
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
    use compio::buf::BufResult;

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
    use compio::buf::{BufResult, IoBuf, IoBufMut};
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
        let reason_len = reason.len().min(255);
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

        let ct = salsa_encrypt(bob.public.as_bytes(), &alice.secret.to_raw_bytes(), &nonce, pt).unwrap();
        let recovered = salsa_decrypt(alice.public.as_bytes(), &bob.secret.to_raw_bytes(), &nonce, &ct).unwrap();

        assert_eq!(recovered, pt);
    }

    #[test]
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

        assert_eq!(client_key, server_key, "both sides must derive the same message key");
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
        let mut client = CurveClient::new(client_keypair, server_public);
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
        let mut server = CurveServer::new(server_keypair);
        server.message_box = Some(CurveBox::new(&shared_secret));

        let plaintext = server.decrypt_message(&frame).unwrap();
        assert_eq!(plaintext.as_ref(), b"client message");
    }

    #[test]
    fn decrypt_message_rejects_invalid_curve_message_command_header() {
        let client_keypair = CurveKeyPair::generate();
        let server_public = CurveKeyPair::generate().public;
        let mut client = CurveClient::new(client_keypair, server_public);
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
        let mut client = CurveClient::new(client_keypair, server_public);
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

    #[compio::test]
    async fn test_send_zmtp_error_retries_short_writes() {
        assert_zmtp_error_survives_partial_writes([2, 3]).await;
    }

    #[compio::test]
    async fn test_send_zmtp_error_retries_short_body_writes() {
        assert_zmtp_error_survives_partial_writes([9, 2]).await;
    }
}
