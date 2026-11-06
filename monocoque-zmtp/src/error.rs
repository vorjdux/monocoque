//! Errors raised while greeting, handshaking and authenticating a ZMTP peer.

use std::io;
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
