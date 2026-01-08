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
use compio::io::{AsyncReadExt, AsyncWriteExt};
use compio::net::TcpStream;
use monocoque_core::alloc::IoBytes;
use tracing::debug;

/// Result of a successful handshake
#[derive(Debug)]
pub struct HandshakeResult {
    pub peer_identity: Option<Bytes>,
    pub peer_socket_type: SocketType,
}

/// Performs the complete ZMTP handshake synchronously on the stream.
///
/// This function blocks until:
/// 1. Greeting exchange is complete
/// 2. READY command exchange is complete
///
/// Only after this completes should the stream be handed to `SocketActor`.
pub async fn perform_handshake(
    stream: &mut TcpStream,
    local_socket_type: SocketType,
    identity: Option<&[u8]>,
) -> Result<HandshakeResult, ZmtpError> {
    debug!("[HANDSHAKE] Starting synchronous handshake for {}", local_socket_type.as_str());
    
    // Step 1: Send our greeting
    let greeting_bytes = build_greeting();
    let io_buf = IoBytes::new(greeting_bytes.clone());
    let BufResult(write_res, _) = stream.write_all(io_buf).await;
    write_res.map_err(|_| ZmtpError::Protocol)?;
    debug!("[HANDSHAKE] Sent greeting ({} bytes)", greeting_bytes.len());

    // Step 2: Receive peer greeting
    let greeting_buf = [0u8; 64];
    let BufResult(read_res, greeting_buf) = stream.read_exact(greeting_buf).await;
    read_res.map_err(|_| ZmtpError::Protocol)?;
    debug!("[HANDSHAKE] Received peer greeting (64 bytes)");
    
    // Validate greeting
    if greeting_buf[0] != 0xFF {
        return Err(ZmtpError::Protocol);
    }

    // Step 3: Send READY command
    let ready_body = build_ready(local_socket_type.as_str(), identity);
    let ready_frame = encode_frame(FLAG_COMMAND, &ready_body);
    let io_buf = IoBytes::new(ready_frame.clone());
    let BufResult(write_res, _) = stream.write_all(io_buf).await;
    write_res.map_err(|_| ZmtpError::Protocol)?;
    debug!("[HANDSHAKE] Sent READY command ({} bytes)", ready_frame.len());

    // Step 4: Receive peer READY command
    // Read the frame header first
    let header_buf = [0u8; 2];
    let BufResult(read_res, header_buf) = stream.read_exact(header_buf).await;
    read_res.map_err(|_| ZmtpError::Protocol)?;
    
    let flags = header_buf[0];
    let is_command = (flags & FLAG_COMMAND) != 0;
    let is_long = (flags & 0x02) != 0;
    
    if !is_command {
        debug!("[HANDSHAKE] ERROR: Expected COMMAND frame, got data frame");
        return Err(ZmtpError::Protocol);
    }

    // Read body length
    let body_len = if is_long {
        let len_buf = [0u8; 8];
        let BufResult(read_res, len_buf) = stream.read_exact(len_buf).await;
        read_res.map_err(|_| ZmtpError::Protocol)?;
        u64::from_be_bytes(len_buf) as usize
    } else {
        header_buf[1] as usize
    };

    // Read body
    // READY commands are typically small (~27 bytes), use stack buffer
    const MAX_READY_SIZE: usize = 512; // Generous limit for READY command
    if body_len > MAX_READY_SIZE {
        debug!("[HANDSHAKE] ERROR: READY body too large: {} bytes", body_len);
        return Err(ZmtpError::Protocol);
    }
    let body_buf = vec![0u8; body_len];
    let BufResult(read_res, body_buf) = stream.read_exact(body_buf).await;
    read_res.map_err(|_| ZmtpError::Protocol)?;
    
    debug!("[HANDSHAKE] Received READY command ({} total bytes)", 2 + if is_long { 8 } else { 0 } + body_len);

    // Parse READY command
    let ready_bytes = Bytes::from(body_buf);
    let (peer_socket_type, peer_identity) = parse_ready_command(&ready_bytes)?;
    
    debug!("[HANDSHAKE] Handshake complete! Peer is {}", peer_socket_type.as_str());

    Ok(HandshakeResult {
        peer_identity,
        peer_socket_type,
    })
}

/// Build a ZMTP 3.0 greeting (64 bytes)
fn build_greeting() -> Bytes {
    let mut b = BytesMut::with_capacity(64);

    // Signature
    b.extend_from_slice(&[0xFF]);
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&[0x7F]);

    // Version 3.0
    b.extend_from_slice(&[0x03, 0x00]);

    // Mechanism: NULL
    b.extend_from_slice(b"NULL");
    b.extend_from_slice(&[0u8; 16]);

    // As-server flag = 0
    b.extend_from_slice(&[0x00]);

    // Padding
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
        return Err(ZmtpError::Protocol);
    }

    let name_len = body[0] as usize;
    if name_len != 5 || &body[1..6] != b"READY" {
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

    let socket_type = socket_type.ok_or(ZmtpError::Protocol)?;
    Ok((socket_type, identity))
}

/// Parse socket type from bytes
const fn parse_socket_type(value: &[u8]) -> Result<SocketType, ZmtpError> {
    match value {
        b"PAIR" => Ok(SocketType::Pair),
        b"DEALER" => Ok(SocketType::Dealer),
        b"ROUTER" => Ok(SocketType::Router),
        b"PUB" => Ok(SocketType::Pub),
        b"SUB" => Ok(SocketType::Sub),
        b"REQ" => Ok(SocketType::Req),
        b"REP" => Ok(SocketType::Rep),
        b"PUSH" => Ok(SocketType::Push),
        b"PULL" => Ok(SocketType::Pull),
        _ => Err(ZmtpError::Protocol),
    }
}
