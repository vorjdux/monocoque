//! Socket type enumeration for ZeroMQ socket types.
//!
//! This module provides the `SocketType` enum which represents the different
//! types of ZeroMQ sockets according to ZMTP 3.1 specification.

use std::fmt;

/// ZeroMQ socket types.
///
/// Corresponds to ZMQ_TYPE socket option (16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SocketType {
    /// PAIR socket for exclusive bidirectional communication
    Pair = 0,
    
    /// PUB socket for publishing messages to subscribers
    Pub = 1,
    
    /// SUB socket for subscribing to published messages
    Sub = 2,
    
    /// REQ socket for synchronous request-reply client
    Req = 3,
    
    /// REP socket for synchronous request-reply server
    Rep = 4,
    
    /// DEALER socket for asynchronous request-reply patterns
    Dealer = 5,
    
    /// ROUTER socket for routing messages by identity
    Router = 6,
    
    /// PULL socket for receiving messages from pushers
    Pull = 7,
    
    /// PUSH socket for sending messages to pullers
    Push = 8,
    
    /// XPUB socket for extended publisher with subscription awareness
    XPub = 9,
    
    /// XSUB socket for extended subscriber with dynamic subscriptions
    XSub = 10,
    
    /// STREAM socket for raw TCP connections (not yet implemented)
    Stream = 11,
}

impl SocketType {
    /// Get the socket type as a string name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pair => "PAIR",
            Self::Pub => "PUB",
            Self::Sub => "SUB",
            Self::Req => "REQ",
            Self::Rep => "REP",
            Self::Dealer => "DEALER",
            Self::Router => "ROUTER",
            Self::Pull => "PULL",
            Self::Push => "PUSH",
            Self::XPub => "XPUB",
            Self::XSub => "XSUB",
            Self::Stream => "STREAM",
        }
    }
    
    /// Check if this socket type is compatible with the given peer type.
    pub fn is_compatible(&self, peer: SocketType) -> bool {
        matches!(
            (self, peer),
            (Self::Pair, Self::Pair)
                | (Self::Pub, Self::Sub)
                | (Self::Sub, Self::Pub)
                | (Self::Req, Self::Rep)
                | (Self::Rep, Self::Req)
                | (Self::Req, Self::Router)
                | (Self::Router, Self::Req)
                | (Self::Dealer, Self::Rep)
                | (Self::Rep, Self::Dealer)
                | (Self::Dealer, Self::Router)
                | (Self::Router, Self::Dealer)
                | (Self::Dealer, Self::Dealer)
                | (Self::Router, Self::Router)
                | (Self::Push, Self::Pull)
                | (Self::Pull, Self::Push)
                | (Self::XPub, Self::XSub)
                | (Self::XSub, Self::XPub)
        )
    }
}

impl fmt::Display for SocketType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_type_display() {
        assert_eq!(SocketType::Dealer.to_string(), "DEALER");
        assert_eq!(SocketType::Router.to_string(), "ROUTER");
        assert_eq!(SocketType::Pub.to_string(), "PUB");
    }

    #[test]
    fn test_socket_compatibility() {
        assert!(SocketType::Req.is_compatible(SocketType::Rep));
        assert!(SocketType::Rep.is_compatible(SocketType::Req));
        assert!(SocketType::Dealer.is_compatible(SocketType::Router));
        assert!(SocketType::Router.is_compatible(SocketType::Dealer));
        assert!(SocketType::Push.is_compatible(SocketType::Pull));
        assert!(SocketType::Pub.is_compatible(SocketType::Sub));
        assert!(SocketType::XPub.is_compatible(SocketType::XSub));
        
        // Incompatible pairs
        assert!(!SocketType::Req.is_compatible(SocketType::Dealer));
        assert!(!SocketType::Pub.is_compatible(SocketType::Pull));
    }
}
