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
    #[allow(dead_code)]
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
        if src.len() != GREETING_SIZE {
            return Err(ZmtpError::Protocol);
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
        let mech_end = mechanism.iter().position(|&b| b == 0).unwrap_or(20);
        if mech_end == 0
            || !mechanism[..mech_end]
                .iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            || mechanism[mech_end..].iter().any(|&b| b != 0)
        {
            return Err(ZmtpError::Protocol);
        }
        if src[32] > 1 {
            return Err(ZmtpError::Protocol);
        }
        let as_server = src[32] != 0;
        Ok(Self {
            mechanism,
            as_server,
        })
    }

    /// Returns the mechanism name as a trimmed string (strips NUL padding).
    pub fn mechanism_str(&self) -> &str {
        let end = self.mechanism.iter().position(|&b| b == 0).unwrap_or(20);
        std::str::from_utf8(&self.mechanism[..end]).unwrap_or("UNKNOWN")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_greeting() -> Bytes {
        let mut greeting = [0u8; GREETING_SIZE];
        greeting[0] = SIGNATURE_HEAD;
        greeting[9] = SIGNATURE_TAIL;
        greeting[10] = 3;
        greeting[11] = 1;
        greeting[12..16].copy_from_slice(b"NULL");
        Bytes::copy_from_slice(&greeting)
    }

    #[test]
    fn parse_rejects_trailing_bytes_after_fixed_greeting() {
        let mut greeting = valid_greeting().to_vec();
        greeting.push(0);

        assert!(matches!(
            ZmtpGreeting::parse(&Bytes::from(greeting)),
            Err(ZmtpError::Protocol)
        ));
    }

    #[test]
    fn parse_rejects_invalid_as_server_flag() {
        let mut greeting = valid_greeting().to_vec();
        greeting[32] = 2;

        assert!(matches!(
            ZmtpGreeting::parse(&Bytes::from(greeting)),
            Err(ZmtpError::Protocol)
        ));
    }

    #[test]
    fn parse_rejects_invalid_security_mechanism_characters() {
        let mut greeting = valid_greeting().to_vec();
        greeting[12..18].copy_from_slice(b"BAD ME");

        assert!(matches!(
            ZmtpGreeting::parse(&Bytes::from(greeting)),
            Err(ZmtpError::Protocol)
        ));
    }

    #[test]
    fn parse_rejects_nonzero_mechanism_padding() {
        let mut greeting = valid_greeting().to_vec();
        greeting[17..22].copy_from_slice(b"CURVE");

        assert!(matches!(
            ZmtpGreeting::parse(&Bytes::from(greeting)),
            Err(ZmtpError::Protocol)
        ));
    }
}
