use crate::codec::ZmtpError;
use bytes::Bytes;

/// ZMTP Greeting is always exactly 64 bytes
pub const GREETING_SIZE: usize = 64;

const SIGNATURE_HEAD: u8 = 0xFF;
const SIGNATURE_TAIL: u8 = 0x7F;

/// Parsed greeting information from peer.
#[derive(Debug, Clone)]
pub struct ZmtpGreeting {
    /// Security mechanism advertised by the peer (e.g., "NULL", "PLAIN", "CURVE")
    pub mechanism: [u8; 20],
    /// Whether the peer is acting as server for the security mechanism
    pub as_server: bool,
}

impl ZmtpGreeting {
    /// Validate and parse a 64-byte ZMTP greeting.
    ///
    /// Layout (ZMTP 3.x):
    /// ```text
    /// [0]      0xFF
    /// [1..9]   Padding
    /// [9]      0x7F
    /// [10]     Major version
    /// [11]     Minor version
    /// [12..32] Mechanism (ASCII, null-padded)
    /// [32]     As-Server flag
    /// [33..64] Padding
    /// ```
    ///
    /// # Compatibility
    ///
    /// Accepts any ZMTP 3.x version (3.0, 3.1, etc.), ensuring compatibility with:
    /// - `ZeroMQ` 4.1+ (ZMTP 3.0)
    /// - `ZeroMQ` 4.2+ (ZMTP 3.1)
    /// - `ZeroMQ` 4.3+ (ZMTP 3.1)
    ///
    /// This provides backward and forward compatibility across all modern ZMQ versions.
    pub fn parse(src: &Bytes) -> crate::codec::Result<Self> {
        if src.len() < GREETING_SIZE {
            return Err(ZmtpError::Incomplete);
        }
        if src[0] != SIGNATURE_HEAD || src[9] != SIGNATURE_TAIL {
            return Err(ZmtpError::Protocol);
        }
        let major = src[10];
        if major < 3 {
            return Err(ZmtpError::Protocol);
        }
        let mut mechanism = [0u8; 20];
        mechanism.copy_from_slice(&src[12..32]);
        let as_server = src[32] != 0;
        Ok(Self { mechanism, as_server })
    }

    /// Returns the mechanism name as a trimmed string (strips NUL padding).
    pub fn mechanism_str(&self) -> &str {
        let end = self.mechanism.iter().position(|&b| b == 0).unwrap_or(20);
        std::str::from_utf8(&self.mechanism[..end]).unwrap_or("UNKNOWN")
    }
}
