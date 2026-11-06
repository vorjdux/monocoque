//! The one error type every public operation returns.
//!
//! Each ZeroMQ error a caller acts on has its own variant rather than hiding in
//! an `io::ErrorKind`: [`Again`](MonocoqueError::Again) for EAGAIN,
//! [`TimedOut`](MonocoqueError::TimedOut) for a receive or send timeout,
//! [`HostUnreachable`](MonocoqueError::HostUnreachable) for a ROUTER with
//! mandatory routing, [`InvalidState`](MonocoqueError::InvalidState) for EFSM,
//! [`Unsupported`](MonocoqueError::Unsupported) for ENOTSUP. Operating-system
//! failures keep their `io::Error` as the source.

use std::io;
use std::time::Duration;
use thiserror::Error;

/// Error returned by every Monocoque operation.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum MonocoqueError {
    /// An operating-system I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The operation would block: a non-blocking call found no message, or a
    /// non-blocking send met the high-water mark (EAGAIN).
    #[error("operation would block")]
    Again,

    /// A receive or send timeout elapsed.
    #[error("operation timed out")]
    TimedOut,

    /// A ROUTER with mandatory routing was asked to send to an unknown peer.
    #[error("no route to peer")]
    HostUnreachable,

    /// The operation is not valid in the socket's current state (EFSM), such
    /// as a REQ sending twice without receiving.
    #[error("invalid socket state: {0}")]
    InvalidState(&'static str),

    /// The socket type does not support the operation (ENOTSUP).
    #[error("operation not supported: {0}")]
    Unsupported(&'static str),

    /// An endpoint string did not parse, or names a transport that is not
    /// available.
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),

    /// The peer violated the wire protocol.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The security handshake rejected the peer or was rejected by it.
    #[error("authentication failed")]
    AuthenticationFailed,

    /// The handshake did not complete within the handshake timeout.
    #[error("handshake timed out after {0:?}")]
    HandshakeTimeout(Duration),

    /// The peer's ZMTP greeting was malformed.
    #[error("invalid greeting: {0}")]
    InvalidGreeting(String),

    /// A frame was malformed.
    #[error("invalid frame: {0}")]
    InvalidFrame(String),

    /// The socket is closed.
    #[error("socket closed")]
    Closed,

    /// The peer went away.
    #[error("peer disconnected: {0}")]
    PeerDisconnected(String),

    /// A routing identity was invalid (empty, too long, or reserved).
    #[error("invalid routing id")]
    InvalidRoutingId,

    /// A message exceeded the configured maximum size.
    #[error("message too large: {size} bytes (max {max})")]
    MessageTooLarge {
        /// Size of the offending message.
        size: usize,
        /// Configured maximum.
        max: usize,
    },

    /// A subscription could not be applied.
    #[error("subscription error: {0}")]
    Subscription(String),
}

/// Result alias for Monocoque operations.
pub type Result<T> = std::result::Result<T, MonocoqueError>;

impl MonocoqueError {
    /// Build a protocol error.
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }

    /// Build an invalid-greeting error.
    pub fn invalid_greeting(msg: impl Into<String>) -> Self {
        Self::InvalidGreeting(msg.into())
    }

    /// Build an invalid-frame error.
    pub fn invalid_frame(msg: impl Into<String>) -> Self {
        Self::InvalidFrame(msg.into())
    }

    /// Build a peer-disconnected error.
    pub fn peer_disconnected(peer_id: impl Into<String>) -> Self {
        Self::PeerDisconnected(peer_id.into())
    }

    /// `true` for EAGAIN: retrying later may succeed.
    #[must_use]
    pub fn is_again(&self) -> bool {
        match self {
            Self::Again => true,
            Self::Io(e) => e.kind() == io::ErrorKind::WouldBlock,
            _ => false,
        }
    }

    /// `true` when a timeout elapsed.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        match self {
            Self::TimedOut | Self::HandshakeTimeout(_) => true,
            Self::Io(e) => e.kind() == io::ErrorKind::TimedOut,
            _ => false,
        }
    }

    /// `true` if the operation may succeed when retried.
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        self.is_again()
            || self.is_timeout()
            || matches!(self, Self::Io(e) if e.kind() == io::ErrorKind::Interrupted)
    }

    /// `true` for errors that mean a connection is gone.
    #[must_use]
    pub const fn is_connection_error(&self) -> bool {
        matches!(
            self,
            Self::Closed | Self::PeerDisconnected(_) | Self::HandshakeTimeout(_)
        )
    }

    /// The closest `io::ErrorKind`, for callers bridging into `io` code.
    #[must_use]
    pub fn io_kind(&self) -> io::ErrorKind {
        match self {
            Self::Io(e) => e.kind(),
            Self::Again => io::ErrorKind::WouldBlock,
            Self::TimedOut | Self::HandshakeTimeout(_) => io::ErrorKind::TimedOut,
            Self::HostUnreachable => io::ErrorKind::HostUnreachable,
            Self::InvalidState(_) => io::ErrorKind::Other,
            Self::Unsupported(_) => io::ErrorKind::Unsupported,
            Self::InvalidEndpoint(_) | Self::InvalidRoutingId => io::ErrorKind::InvalidInput,
            Self::Closed => io::ErrorKind::NotConnected,
            Self::PeerDisconnected(_) => io::ErrorKind::ConnectionReset,
            Self::AuthenticationFailed => io::ErrorKind::PermissionDenied,
            Self::Protocol(_)
            | Self::InvalidGreeting(_)
            | Self::InvalidFrame(_)
            | Self::MessageTooLarge { .. }
            | Self::Subscription(_) => io::ErrorKind::InvalidData,
        }
    }
}

impl From<MonocoqueError> for io::Error {
    fn from(err: MonocoqueError) -> Self {
        match err {
            MonocoqueError::Io(e) => e,
            other => Self::new(other.io_kind(), other),
        }
    }
}

impl From<crate::endpoint::EndpointError> for MonocoqueError {
    fn from(err: crate::endpoint::EndpointError) -> Self {
        Self::InvalidEndpoint(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn again_and_timeout_classify_both_native_and_io_forms() {
        assert!(MonocoqueError::Again.is_again());
        assert!(MonocoqueError::from(io::Error::from(io::ErrorKind::WouldBlock)).is_again());
        assert!(MonocoqueError::TimedOut.is_timeout());
        assert!(!MonocoqueError::Closed.is_recoverable());
    }

    #[test]
    fn converts_to_io_error_with_matching_kind() {
        let e: io::Error = MonocoqueError::HostUnreachable.into();
        assert_eq!(e.kind(), io::ErrorKind::HostUnreachable);
        let e: io::Error = MonocoqueError::Io(io::Error::from(io::ErrorKind::AddrInUse)).into();
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
    }
}
