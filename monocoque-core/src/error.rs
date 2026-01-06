/// Monocoque Error Types
///
/// Comprehensive error handling for all Monocoque operations.

use std::io;
use thiserror::Error;

/// Main error type for Monocoque operations
#[derive(Error, Debug)]
pub enum MonocoqueError {
    /// IO error during socket operations
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    
    /// Protocol error during ZMTP handshake or framing
    #[error("Protocol error: {0}")]
    Protocol(String),
    
    /// Handshake timeout
    #[error("Handshake timeout after {0:?}")]
    HandshakeTimeout(std::time::Duration),
    
    /// Invalid greeting received
    #[error("Invalid greeting: {0}")]
    InvalidGreeting(String),
    
    /// Invalid frame format
    #[error("Invalid frame: {0}")]
    InvalidFrame(String),
    
    /// Socket closed
    #[error("Socket closed")]
    SocketClosed,
    
    /// Channel send error
    #[error("Channel send error")]
    ChannelSend,
    
    /// Channel receive error
    #[error("Channel receive error")]
    ChannelRecv,
    
    /// Peer disconnected
    #[error("Peer disconnected: {0}")]
    PeerDisconnected(String),
    
    /// Invalid routing ID
    #[error("Invalid routing ID")]
    InvalidRoutingId,
    
    /// Message too large
    #[error("Message too large: {size} bytes (max: {max})")]
    MessageTooLarge { size: usize, max: usize },
    
    /// Subscription error
    #[error("Subscription error: {0}")]
    Subscription(String),
}

/// Result type alias for Monocoque operations
pub type Result<T> = std::result::Result<T, MonocoqueError>;

impl MonocoqueError {
    /// Create a protocol error with a message
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }
    
    /// Create an invalid greeting error
    pub fn invalid_greeting(msg: impl Into<String>) -> Self {
        Self::InvalidGreeting(msg.into())
    }
    
    /// Create an invalid frame error
    pub fn invalid_frame(msg: impl Into<String>) -> Self {
        Self::InvalidFrame(msg.into())
    }
    
    /// Create a peer disconnected error
    pub fn peer_disconnected(peer_id: impl Into<String>) -> Self {
        Self::PeerDisconnected(peer_id.into())
    }
    
    /// Check if this error is recoverable
    #[must_use] 
    pub fn is_recoverable(&self) -> bool {
        match self {
            Self::Io(e) => match e.kind() {
                io::ErrorKind::Interrupted
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::TimedOut => true,
                _ => false,
            },
            Self::HandshakeTimeout(_)
            | Self::ChannelSend
            | Self::ChannelRecv => false,
            _ => false,
        }
    }
    
    /// Check if this is a connection error
    #[must_use] 
    pub const fn is_connection_error(&self) -> bool {
        matches!(
            self,
            Self::SocketClosed
                | Self::PeerDisconnected(_)
                | Self::HandshakeTimeout(_)
        )
    }
}
