use bytes::{Buf, Bytes, BytesMut};
use monocoque_core::buffer::SegmentedBuffer;
use std::io;
use thiserror::Error;

use monocoque_core::config::STAGING_BUF_INITIAL_CAP;

/// ZMTP protocol errors
#[derive(Debug, Error)]
pub enum ZmtpError {
    #[error("Incomplete frame")]
    Incomplete,

    #[error("Protocol violation: reserved bits set")]
    ReservedBits,

    #[error("Protocol violation: frame size too large")]
    SizeTooLarge,

    #[error("Protocol violation")]
    Protocol,

    #[error("Authentication failed")]
    AuthenticationFailed,
}

impl From<ZmtpError> for io::Error {
    fn from(err: ZmtpError) -> Self {
        Self::new(io::ErrorKind::InvalidData, err)
    }
}

impl From<io::Error> for ZmtpError {
    fn from(_err: io::Error) -> Self {
        // Convert IO errors to Protocol errors for now
        Self::Protocol
    }
}

/// Result type alias for ZMTP operations
pub type Result<T> = std::result::Result<T, ZmtpError>;

/// A decoded ZMTP frame
#[derive(Debug, Clone)]
pub struct ZmtpFrame {
    pub flags: u8,
    pub payload: Bytes,
}

impl ZmtpFrame {
    #[inline]
    pub const fn more(&self) -> bool {
        (self.flags & 0x01) != 0
    }

    #[inline]
    pub const fn is_command(&self) -> bool {
        (self.flags & 0x04) != 0
    }
}

/// Stateful ZMTP decoder
///
/// Fast path:
/// - Entire frame present → zero-copy slice
///
/// Slow path:
/// - Fragmented frame → reassemble into `BytesMut`
pub struct ZmtpDecoder {
    // Fragmentation state
    pending_flags: Option<u8>,
    expected_body_len: usize,
    staging: BytesMut,
}

impl ZmtpDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending_flags: None,
            expected_body_len: 0,
            staging: BytesMut::with_capacity(STAGING_BUF_INITIAL_CAP),
        }
    }

    /// Check if more message frames are expected (partial multipart message).
    ///
    /// Returns `true` if the decoder is in the middle of reassembling a frame
    /// or if the last decoded frame had the MORE flag set.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) - check if more frames in current message.
    #[inline]
    pub const fn has_more(&self) -> bool {
        // Decoder is expecting more data for current frame
        self.pending_flags.is_some()
    }

    /// Decode a single frame from `src`
    ///
    /// Returns:
    /// - Ok(Some(frame)) → frame decoded
    /// - Ok(None) → need more data
    /// - Err → protocol violation
    pub fn decode(&mut self, src: &mut SegmentedBuffer) -> Result<Option<ZmtpFrame>> {
        // === Reassembly mode ===
        if let Some(flags) = self.pending_flags {
            let needed = self.expected_body_len - self.staging.len();
            let take = needed.min(src.len());
            if let Some(bytes) = src.take_bytes(take) {
                self.staging.extend_from_slice(&bytes);
            }

            if self.staging.len() < self.expected_body_len {
                return Ok(None);
            }

            let payload = self.staging.split().freeze();
            self.pending_flags = None;
            self.expected_body_len = 0;

            return Ok(Some(ZmtpFrame { flags, payload }));
        }

        // === Header parsing ===
        if src.len() < 2 {
            return Ok(None);
        }

        let mut hdr = [0u8; 9];
        if !src.copy_prefix(2, &mut hdr) {
            return Ok(None);
        }

        let flags = hdr[0];

        // Reserved bits must be zero (bits 3–7)
        if (flags & 0xF8) != 0 {
            return Err(ZmtpError::ReservedBits);
        }

        let is_long = (flags & 0x02) != 0;
        let header_len = if is_long { 9 } else { 2 };

        if src.len() < header_len {
            return Ok(None);
        }

        // === Body length ===
        let body_len = if is_long {
            if !src.copy_prefix(9, &mut hdr) {
                return Ok(None);
            }
            let mut buf = &hdr[1..9];
            let size = buf.get_u64();

            // MSB must be zero in ZMTP 3.x
            if size > 0x7FFF_FFFF_FFFF_FFFF {
                return Err(ZmtpError::SizeTooLarge);
            }

            size as usize
        } else {
            hdr[1] as usize
        };

        let total_len = header_len + body_len;

        // === Fast path: entire frame present ===
        if src.len() >= total_len {
            src.advance(header_len);
            let payload = src
                .take_bytes(body_len)
                .expect("len check ensures body is available");
            return Ok(Some(ZmtpFrame { flags, payload }));
        }

        // === Slow path: fragmentation ===
        src.advance(header_len);
        self.pending_flags = Some(flags);
        self.expected_body_len = body_len;
        self.staging.clear();

        let available = src.len().min(body_len);
        if let Some(bytes) = src.take_bytes(available) {
            self.staging.extend_from_slice(&bytes);
        }

        Ok(None)
    }
}

impl ZmtpFrame {
    /// Create a data frame
    pub const fn data(payload: Bytes, more: bool) -> Self {
        let mut flags = 0;
        if more {
            flags |= 0x01; // MORE
        }
        if payload.len() > 255 {
            flags |= 0x02; // LONG
        }
        Self { flags, payload }
    }

    /// Create a command frame
    pub const fn command(payload: Bytes) -> Self {
        let mut flags = 0x04; // COMMAND
        if payload.len() > 255 {
            flags |= 0x02; // LONG
        }
        Self { flags, payload }
    }

    /// Encode this frame to bytes
    pub fn encode(&self) -> Bytes {
        let is_long = (self.flags & 0x02) != 0;
        let body_len = self.payload.len();

        let mut out = BytesMut::with_capacity(if is_long { 9 } else { 2 } + body_len);

        out.extend_from_slice(&[self.flags]);

        if is_long {
            out.extend_from_slice(&(body_len as u64).to_be_bytes());
        } else {
            out.extend_from_slice(&[body_len as u8]);
        }

        out.extend_from_slice(&self.payload);

        out.freeze()
    }
}

/// Encode a multipart message directly into a buffer.
///
/// This is a zero-allocation helper for encoding messages without
/// creating intermediate `ZmtpFrame` objects.
///
/// # Performance
///
/// Reuses the provided `BytesMut` buffer, avoiding allocations on hot path.
pub fn encode_multipart(msg: &[Bytes], buf: &mut BytesMut) {
    if msg.is_empty() {
        return;
    }

    // Fast path: single-frame message (common case)
    if msg.len() == 1 {
        let part = &msg[0];
        let is_long = part.len() >= 256;
        let flags = if is_long { 0x02 } else { 0x00 }; // No MORE flag

        buf.reserve(if is_long { 9 } else { 2 } + part.len());
        buf.extend_from_slice(&[flags]);

        if is_long {
            buf.extend_from_slice(&(part.len() as u64).to_be_bytes());
        } else {
            buf.extend_from_slice(&[part.len() as u8]);
        }

        buf.extend_from_slice(part);
        return;
    }

    // Multi-frame path
    for (i, part) in msg.iter().enumerate() {
        let more = i < msg.len() - 1;
        let is_long = part.len() >= 256;

        let mut flags = 0u8;
        if more {
            flags |= 0x01; // MORE
        }
        if is_long {
            flags |= 0x02; // LONG
        }

        buf.reserve(if is_long { 9 } else { 2 } + part.len());
        buf.extend_from_slice(&[flags]);

        if is_long {
            buf.extend_from_slice(&(part.len() as u64).to_be_bytes());
        } else {
            buf.extend_from_slice(&[part.len() as u8]);
        }

        buf.extend_from_slice(part);
    }
}
