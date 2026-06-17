//! Synchronous ZMTP handshake that completes before spawning background tasks.
//!
//! This eliminates race conditions by ensuring both peers complete the handshake
//! protocol before any application data can be sent.
//!
//! ## Memory Allocation Strategy
//!
//! This module uses **stack arrays** for all fixed-size protocol buffers:
//! - Greeting: 64-byte stack array
//! - Frame header: 2-byte stack array
//! - Length field: 8-byte stack array
//!
//! The READY body uses a small `Vec` allocation (typically ~27 bytes) because:
//! 1. compio's ownership-passing API requires owned buffers (can't use &mut slice)
//! 2. Size is dynamic but bounded (max 512 bytes enforced)
//! 3. Handshake happens once per connection (not in hot path)
//! 4. Total allocation overhead: ~93 bytes one-time per connection
//!
//! After handshake completes, the main data path uses arena allocator for zero-copy IO.

use crate::codec::ZmtpError;
use crate::session::SocketType;
use crate::utils::{build_ready, encode_frame, FLAG_COMMAND};
use bytes::{Bytes, BytesMut};
use compio::buf::BufResult;
use compio::io::{AsyncRead, AsyncWrite};
use monocoque_core::options::SocketOptions;
use monocoque_core::timeout::{read_exact_with_timeout, write_all_with_timeout};
use std::time::Duration;
use tracing::{debug, warn};

/// Result of a successful handshake
#[derive(Debug)]
pub struct HandshakeResult {
    pub peer_identity: Option<Bytes>,
    pub peer_socket_type: SocketType,
}

/// Security mechanism to use for the ZMTP handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityMechanism {
    /// No authentication (default).
    Null,
    /// Username/password authentication (PLAIN).
    Plain,
    /// Public-key encryption (CURVE).
    Curve,
}

impl SecurityMechanism {
    /// Detect the mechanism from socket options.
    ///
    /// Priority: CURVE > PLAIN > NULL.
    pub fn from_options(options: &SocketOptions) -> Self {
        if options.curve_secretkey.is_some() || options.curve_server {
            Self::Curve
        } else if options.plain_server || options.plain_username.is_some() {
            Self::Plain
        } else {
            Self::Null
        }
    }

    /// The ASCII mechanism name used in ZMTP greetings (20-byte field).
    pub fn as_greeting_bytes(&self) -> &'static [u8] {
        match self {
            Self::Null => b"NULL",
            Self::Plain => b"PLAIN",
            Self::Curve => b"CURVE",
        }
    }
}

/// Performs the complete ZMTP handshake, selecting the security mechanism from options.
///
/// This is the primary handshake entry point for sockets that have security configured.
pub async fn perform_handshake_with_options<S>(
    stream: &mut S,
    local_socket_type: SocketType,
    identity: Option<&[u8]>,
    timeout: Option<Duration>,
    options: &SocketOptions,
) -> Result<HandshakeResult, ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mechanism = SecurityMechanism::from_options(options);

    debug!(
        "[HANDSHAKE] Starting handshake for {} (timeout: {:?}, mechanism: {:?})",
        local_socket_type.as_str(),
        timeout,
        mechanism
    );

    // Step 1: Send our greeting
    debug!("[HANDSHAKE] Step 1: Sending greeting...");
    let greeting_bytes = build_greeting_with_mechanism(mechanism, options);
    let BufResult(write_res, _) = write_all_with_timeout(stream, greeting_bytes.clone(), timeout)
        .await
        .map_err(|e| {
            warn!("[HANDSHAKE] Step 1: Failed to send ZMTP greeting: {}", e);
            ZmtpError::Protocol
        })?;
    write_res.map_err(|e| {
        warn!("[HANDSHAKE] Step 1: Failed to write ZMTP greeting bytes: {}", e);
        ZmtpError::Protocol
    })?;
    debug!(
        "[HANDSHAKE] Step 1 DONE: Sent greeting ({} bytes)",
        greeting_bytes.len()
    );

    // Step 2: Receive peer greeting
    debug!("[HANDSHAKE] Step 2: Receiving peer greeting...");
    let greeting_buf = [0u8; 64];
    let BufResult(read_res, greeting_buf) = read_exact_with_timeout(stream, greeting_buf, timeout)
        .await
        .map_err(|e| {
            warn!("[HANDSHAKE] Step 2: Failed to receive ZMTP greeting: {}", e);
            ZmtpError::Protocol
        })?;
    read_res.map_err(|e| {
        warn!("[HANDSHAKE] Step 2: Failed to read ZMTP greeting bytes: {}", e);
        ZmtpError::Protocol
    })?;
    debug!("[HANDSHAKE] Step 2 DONE: Received peer greeting (64 bytes)");

    // Validate greeting signature
    if greeting_buf[0] != 0xFF {
        warn!(
            "[HANDSHAKE] ZMTP greeting: expected signature byte 0xff at offset 0, got 0x{:02x}",
            greeting_buf[0]
        );
        return Err(ZmtpError::Protocol);
    }

    // Step 3: Run security-mechanism-specific exchange (between greeting and READY)
    match mechanism {
        SecurityMechanism::Null => {
            // No mechanism-level exchange for NULL; proceed directly to READY.
        }
        SecurityMechanism::Plain => {
            run_plain_exchange(stream, options, timeout).await?;
        }
        SecurityMechanism::Curve => {
            run_curve_exchange(stream, options, timeout).await?;
        }
    }

    // Step 4: Send READY command
    debug!("[HANDSHAKE] Step 4: Sending READY command...");
    let ready_body = build_ready(local_socket_type.as_str(), identity);
    let ready_frame = encode_frame(FLAG_COMMAND, &ready_body);
    let BufResult(write_res, _) = write_all_with_timeout(stream, ready_frame.clone(), timeout)
        .await
        .map_err(|e| {
            warn!("[HANDSHAKE] Step 4: Failed to send ZMTP READY command: {}", e);
            ZmtpError::Protocol
        })?;
    write_res.map_err(|e| {
        warn!("[HANDSHAKE] Step 4: Failed to write ZMTP READY command bytes: {}", e);
        ZmtpError::Protocol
    })?;
    debug!(
        "[HANDSHAKE] Step 4 DONE: Sent READY command ({} bytes)",
        ready_frame.len()
    );

    // Step 5: Receive peer READY command
    debug!("[HANDSHAKE] Step 5: Receiving peer READY command...");
    let header_buf = [0u8; 2];
    let BufResult(read_res, header_buf) = read_exact_with_timeout(stream, header_buf, timeout)
        .await
        .map_err(|e| {
            warn!("[HANDSHAKE] Step 5: Failed to receive ZMTP READY frame header: {}", e);
            ZmtpError::Protocol
        })?;
    read_res.map_err(|e| {
        warn!("[HANDSHAKE] Step 5: Failed to read ZMTP READY frame header bytes: {}", e);
        ZmtpError::Protocol
    })?;
    debug!(
        "[HANDSHAKE] Step 5a DONE: Read header [{:02x}, {:02x}]",
        header_buf[0], header_buf[1]
    );

    let flags = header_buf[0];
    let is_command = (flags & FLAG_COMMAND) != 0;
    let is_long = (flags & 0x02) != 0;

    if !is_command {
        warn!(
            "[HANDSHAKE] ZMTP READY step: expected COMMAND frame (flags & 0x04 != 0), \
             got flags=0x{:02x}  -  peer sent a data frame instead of READY",
            flags
        );
        return Err(ZmtpError::Protocol);
    }

    // Read body length
    let body_len = if is_long {
        let len_buf = [0u8; 8];
        let BufResult(read_res, len_buf) = read_exact_with_timeout(stream, len_buf, timeout)
            .await
            .map_err(|e| {
                warn!("[HANDSHAKE] Step 5: Failed to receive ZMTP READY long-frame length: {}", e);
                ZmtpError::Protocol
            })?;
        read_res.map_err(|e| {
            warn!("[HANDSHAKE] Step 5: Failed to read ZMTP READY long-frame length bytes: {}", e);
            ZmtpError::Protocol
        })?;
        u64::from_be_bytes(len_buf) as usize
    } else {
        header_buf[1] as usize
    };
    debug!("[HANDSHAKE] Step 5b DONE: body_len={}", body_len);

    // Read body
    const MAX_READY_SIZE: usize = 512;
    if body_len > MAX_READY_SIZE {
        warn!(
            "[HANDSHAKE] ZMTP READY body too large: got {} bytes, maximum allowed is {} bytes",
            body_len, MAX_READY_SIZE
        );
        return Err(ZmtpError::Protocol);
    }
    let body_buf = vec![0u8; body_len];
    let BufResult(read_res, body_buf) = read_exact_with_timeout(stream, body_buf, timeout)
        .await
        .map_err(|e| {
            warn!("[HANDSHAKE] Step 5: Failed to receive ZMTP READY body ({} bytes): {}", body_len, e);
            ZmtpError::Protocol
        })?;
    read_res.map_err(|e| {
        warn!("[HANDSHAKE] Step 5: Failed to read ZMTP READY body bytes: {}", e);
        ZmtpError::Protocol
    })?;
    debug!("[HANDSHAKE] Step 5c DONE: Read {} bytes of body", body_len);

    // Parse READY command
    let ready_bytes = Bytes::from(body_buf);
    let (peer_socket_type, peer_identity) = parse_ready_command(&ready_bytes)?;

    debug!(
        "[HANDSHAKE] Handshake complete! Peer is {}",
        peer_socket_type.as_str()
    );

    Ok(HandshakeResult {
        peer_identity,
        peer_socket_type,
    })
}

// ---------------------------------------------------------------------------
// Per-mechanism security exchanges
// ---------------------------------------------------------------------------

/// Run the PLAIN authentication exchange.
///
/// - Client mode: send HELLO, receive WELCOME/ERROR.
/// - Server mode: receive HELLO, validate, send WELCOME/ERROR.
async fn run_plain_exchange<S>(
    stream: &mut S,
    options: &SocketOptions,
    timeout: Option<Duration>,
) -> Result<(), ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use crate::security::plain::{
        plain_client_handshake, plain_server_handshake, PlainCredentials, StaticPlainHandler,
    };

    if options.plain_server {
        debug!("[HANDSHAKE] Running PLAIN server exchange");
        // Use in-process handler: accept any user present in options or reject all.
        // For a proper server you would supply a real PlainAuthHandler; the simplest
        // approach is a StaticPlainHandler pre-loaded with no users (reject everything).
        // Callers that want custom validation should use the security API directly.
        // We build a handler that always rejects  -  real auth should go through ZAP.
        // Use plain_server_handshake with a trivial reject-all handler.
        let handler = StaticPlainHandler::new(); // empty → rejects all
        let domain = options.zap_domain.as_str();
        plain_server_handshake(stream, &handler, domain, "unknown", timeout)
            .await
            .map(|_| ())
    } else if let Some(ref username) = options.plain_username {
        debug!("[HANDSHAKE] Running PLAIN client exchange");
        let password = options.plain_password.as_deref().unwrap_or("");
        let credentials = PlainCredentials::new(username.clone(), password);
        plain_client_handshake(stream, &credentials, timeout).await
    } else {
        // Should not happen (mechanism detection guards this), but be safe.
        Ok(())
    }
}

/// Run the CURVE key-exchange handshake.
///
/// - Client mode: `curve_secretkey` + `curve_serverkey` must be set.
/// - Server mode: `curve_server` flag + `curve_secretkey` must be set.
async fn run_curve_exchange<S>(
    stream: &mut S,
    options: &SocketOptions,
    timeout: Option<Duration>,
) -> Result<(), ZmtpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use crate::security::curve::{
        CurveClient, CurveKeyPair, CurvePublicKey, CurveSecretKey, CurveServer,
    };

    if options.curve_server {
        debug!("[HANDSHAKE] Running CURVE server exchange");
        let secret_bytes = options.curve_secretkey.ok_or_else(|| {
            warn!("[HANDSHAKE] CURVE server mode requires curve_secretkey to be set, but it is missing");
            ZmtpError::Protocol
        })?;
        let server_secret = CurveSecretKey::from_bytes(secret_bytes);
        let server_public = server_secret.public_key();
        let server_keypair = CurveKeyPair::from_keys(server_public, server_secret);

        let mut curve_server = CurveServer::new(server_keypair);
        curve_server.handshake(stream, timeout).await.map(|_| ())
    } else if let (Some(secret_bytes), Some(server_key_bytes)) =
        (options.curve_secretkey, options.curve_serverkey)
    {
        debug!("[HANDSHAKE] Running CURVE client exchange");
        let client_secret = CurveSecretKey::from_bytes(secret_bytes);
        let client_public = client_secret.public_key();
        let client_keypair = CurveKeyPair::from_keys(client_public, client_secret);
        let server_public = CurvePublicKey::from_bytes(server_key_bytes);

        let mut curve_client = CurveClient::new(client_keypair, server_public);
        curve_client.handshake(stream, timeout).await
    } else if options.curve_secretkey.is_some() {
        // curve_secretkey set for a client but curve_serverkey is absent.
        warn!(
            "[HANDSHAKE] CURVE client mode requires both curve_secretkey and curve_serverkey, \
             but curve_serverkey (server's public key) is missing"
        );
        Err(ZmtpError::Protocol)
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Greeting helpers
// ---------------------------------------------------------------------------

/// Build a ZMTP 3.0 greeting (64 bytes) advertising the given security mechanism.
fn build_greeting_with_mechanism(mechanism: SecurityMechanism, options: &SocketOptions) -> Bytes {
    let mut b = BytesMut::with_capacity(64);

    // Signature
    b.extend_from_slice(&[0xFF]);
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&[0x7F]);

    // Version 3.0
    b.extend_from_slice(&[0x03, 0x00]);

    // Mechanism field: 20 bytes, ASCII name padded with NUL
    let mech_name = mechanism.as_greeting_bytes();
    b.extend_from_slice(mech_name);
    let padding = 20usize.saturating_sub(mech_name.len());
    b.extend_from_slice(&vec![0u8; padding]);

    // As-server flag (byte 32): 1 if this side acts as CURVE/PLAIN server
    let as_server = match mechanism {
        SecurityMechanism::Curve => options.curve_server,
        SecurityMechanism::Plain => options.plain_server,
        SecurityMechanism::Null => false,
    };
    b.extend_from_slice(&[u8::from(as_server)]);

    // Padding to reach 64 bytes total
    b.extend_from_slice(&[0u8; 31]);

    b.freeze()
}

/// Parse READY command to extract socket type and identity
fn parse_ready_command(body: &Bytes) -> Result<(SocketType, Option<Bytes>), ZmtpError> {
    // READY format:
    // - 1 byte: command name length
    // - N bytes: "READY"
    // - Properties as key-value pairs

    if body.len() < 6 {
        warn!(
            "[HANDSHAKE] ZMTP READY parse: body too short  -  got {} bytes, need at least 6",
            body.len()
        );
        return Err(ZmtpError::Protocol);
    }

    let name_len = body[0] as usize;
    if name_len != 5 || &body[1..6] != b"READY" {
        warn!(
            "[HANDSHAKE] ZMTP READY parse: expected command name \"READY\" (length=5), \
             got length={} name={:?}",
            name_len,
            body.get(1..1 + name_len.min(body.len().saturating_sub(1)))
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default()
        );
        return Err(ZmtpError::Protocol);
    }

    // Parse properties
    let mut offset = 6;
    let mut socket_type = None;
    let mut identity = None;

    while offset < body.len() {
        if offset + 1 > body.len() {
            break;
        }

        let key_len = body[offset] as usize;
        offset += 1;

        if offset + key_len > body.len() {
            break;
        }

        let key = &body[offset..offset + key_len];
        offset += key_len;

        if offset + 4 > body.len() {
            break;
        }

        let value_len = u32::from_be_bytes([
            body[offset],
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + value_len > body.len() {
            break;
        }

        // Store the range for zero-copy slice
        let value_start = offset;
        let value_end = offset + value_len;
        offset += value_len;

        match key {
            b"Socket-Type" => {
                socket_type = Some(parse_socket_type(&body[value_start..value_end])?);
            }
            b"Identity" => {
                // Zero-copy: slice the existing Bytes instead of copying
                identity = Some(body.slice(value_start..value_end));
            }
            _ => {
                // Ignore unknown properties
            }
        }
    }

    let socket_type = socket_type.ok_or_else(|| {
        warn!("[HANDSHAKE] ZMTP READY parse: peer READY command is missing the required \"Socket-Type\" property");
        ZmtpError::Protocol
    })?;
    Ok((socket_type, identity))
}

/// Parse socket type from bytes
fn parse_socket_type(value: &[u8]) -> Result<SocketType, ZmtpError> {
    match value {
        b"PAIR" => Ok(SocketType::Pair),
        b"DEALER" => Ok(SocketType::Dealer),
        b"ROUTER" => Ok(SocketType::Router),
        b"PUB" => Ok(SocketType::Pub),
        b"SUB" => Ok(SocketType::Sub),
        b"XPUB" => Ok(SocketType::Xpub),
        b"XSUB" => Ok(SocketType::Xsub),
        b"REQ" => Ok(SocketType::Req),
        b"REP" => Ok(SocketType::Rep),
        b"PUSH" => Ok(SocketType::Push),
        b"PULL" => Ok(SocketType::Pull),
        _ => {
            warn!(
                "[HANDSHAKE] ZMTP READY parse: unknown Socket-Type value {:?}",
                String::from_utf8_lossy(value)
            );
            Err(ZmtpError::Protocol)
        }
    }
}
