use crate::codec::ZmtpError;
use bytes::Bytes;

/// ZMTP Greeting is always exactly 64 bytes
pub const GREETING_SIZE: usize = 64;

const SIGNATURE_HEAD: u8 = 0xFF;
const SIGNATURE_TAIL: u8 = 0x7F;

/// Supported security mechanisms
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mechanism {
    Null,
    Plain,
    Curve,
    Unknown(String),
}

/// Parsed greeting information
#[derive(Debug, Clone)]
pub struct ZmtpGreeting {
    pub mechanism: Mechanism,
    pub as_server: bool,
}

impl ZmtpGreeting {
    /// Parse a 64-byte ZMTP greeting
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

        // Signature
        if src[0] != SIGNATURE_HEAD || src[9] != SIGNATURE_TAIL {
            return Err(ZmtpError::Protocol);
        }

        // Version (require 3.x for ZMQ 4.1+ compatibility)
        let major = src[10];
        if major < 3 {
            return Err(ZmtpError::Protocol);
        }

        // Mechanism (bytes 12..32)
        let mech_raw = &src[12..32];
        let mech_str = match std::str::from_utf8(mech_raw) {
            Ok(s) => s.trim_matches(char::from(0)),
            Err(_) => return Err(ZmtpError::Protocol),
        };

        let mechanism = match mech_str {
            "NULL" => Mechanism::Null,
            "PLAIN" => Mechanism::Plain,
            "CURVE" => Mechanism::Curve,
            other => Mechanism::Unknown(other.to_string()),
        };

        // As-Server flag (bit 0)
        let as_server = (src[32] & 0x01) != 0;

        Ok(Self {
            mechanism,
            as_server,
        })
    }
}
