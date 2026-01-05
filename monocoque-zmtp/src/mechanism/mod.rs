pub mod null;

use bytes::Bytes;

use crate::codec::{ZmtpError, ZmtpFrame};
use crate::session::SocketType;

/// Role of this endpoint (client/server) for handshake behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

/// Trait implemented by each security mechanism (NULL now, CURVE later).
///
/// The mechanism is responsible for:
/// - validating inbound command frames during handshake
/// - emitting outbound handshake frames (READY, ERROR, etc.)
/// - providing the peer metadata at handshake completion
pub trait Mechanism: Send {
    /// Feed an inbound frame (expected to be command frames during handshake).
    ///
    /// Returns:
    /// - Ok(()) for accepted frames
    /// - Err for protocol/handshake violation
    fn on_inbound(&mut self, frame: &ZmtpFrame) -> Result<(), ZmtpError>;

    /// Poll next outbound bytes to send (already framed bytes).
    ///
    /// Convention:
    /// - returns Some(Bytes) when it has something to send now
    /// - returns None when nothing pending
    fn next_outbound(&mut self) -> Option<Bytes>;

    /// Whether the handshake is finished (mechanism satisfied).
    fn is_done(&self) -> bool;

    /// Peer identity if known (ROUTER mapping). Must be **owned stable bytes**.
    ///
    /// Important:
    /// - This must not point into a slab that might be recycled.
    /// - So mechanisms should store it as owned `Bytes` (usually copy from READY prop).
    fn peer_identity(&self) -> Option<Bytes>;

    /// Peer socket type determined from READY.
    fn peer_socket_type(&self) -> Option<SocketType>;
}

/// Mechanism selection from greeting / config.
///
/// For Phase 1/2 we only support NULL.
/// CURVE can be added later without changing session logic.
pub enum MechanismKind {
    Null,
    // Curve,
    // Plain,
}

impl MechanismKind {
    pub fn new_null() -> Self {
        Self::Null
    }

    pub fn build(self, role: Role, local_socket_type: SocketType) -> Box<dyn Mechanism> {
        match self {
            MechanismKind::Null => Box::new(crate::mechanism::null::NullMechanism::new(
                role,
                local_socket_type,
            )),
        }
    }
}

/// Helper: in handshake, any non-command data frame is a violation.
/// (libzmq will drop you silently if you violate.)
#[inline]
pub fn require_command(frame: &ZmtpFrame) -> Result<(), ZmtpError> {
    if frame.is_command() {
        Ok(())
    } else {
        Err(ZmtpError::Protocol)
    }
}
