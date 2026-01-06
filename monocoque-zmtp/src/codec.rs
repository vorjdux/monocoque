use bytes::{Buf, Bytes, BytesMut};
use thiserror::Error;

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
            staging: BytesMut::new(),
        }
    }

    /// Decode a single frame from `src`
    ///
    /// Returns:
    /// - Ok(Some(frame)) → frame decoded
    /// - Ok(None) → need more data
    /// - Err → protocol violation
    pub fn decode(&mut self, src: &mut Bytes) -> Result<Option<ZmtpFrame>> {
        // === Reassembly mode ===
        if let Some(flags) = self.pending_flags {
            let needed = self.expected_body_len - self.staging.len();
            let take = needed.min(src.len());

            self.staging.extend_from_slice(&src.split_to(take));

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

        let flags = src[0];

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
            let mut buf = &src[1..9];
            let size = buf.get_u64();

            // MSB must be zero in ZMTP 3.x
            if size > 0x7FFF_FFFF_FFFF_FFFF {
                return Err(ZmtpError::SizeTooLarge);
            }

            size as usize
        } else {
            src[1] as usize
        };

        let total_len = header_len + body_len;

        // === Fast path: entire frame present ===
        if src.len() >= total_len {
            src.advance(header_len);
            let payload = src.split_to(body_len);
            return Ok(Some(ZmtpFrame { flags, payload }));
        }

        // === Slow path: fragmentation ===
        src.advance(header_len);
        self.pending_flags = Some(flags);
        self.expected_body_len = body_len;
        self.staging.clear();

        let available = src.len().min(body_len);
        self.staging.extend_from_slice(&src.split_to(available));

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
