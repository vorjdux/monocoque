//! Socket event monitoring.
//!
//! Provides event streams for tracking socket lifecycle events like
//! connections, disconnections, and errors.

use crate::endpoint::Endpoint;
use std::fmt;

/// Socket lifecycle events.
#[derive(Debug, Clone)]
pub enum SocketEvent {
    /// Socket successfully connected to a peer.
    Connected(Endpoint),

    /// Socket disconnected from a peer.
    Disconnected(Endpoint),

    /// Socket successfully bound to an endpoint.
    Bound(Endpoint),

    /// Bind operation failed.
    BindFailed {
        endpoint: Endpoint,
        reason: String,
    },

    /// Connection attempt failed.
    ConnectFailed {
        endpoint: Endpoint,
        reason: String,
    },

    /// Socket is listening for incoming connections.
    Listening(Endpoint),

    /// Socket accepted a new incoming connection.
    Accepted(Endpoint),
}

impl fmt::Display for SocketEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connected(ep) => write!(f, "Connected to {ep}"),
            Self::Disconnected(ep) => write!(f, "Disconnected from {ep}"),
            Self::Bound(ep) => write!(f, "Bound to {ep}"),
            Self::BindFailed { endpoint, reason } => {
                write!(f, "Bind failed for {endpoint}: {reason}")
            }
            Self::ConnectFailed { endpoint, reason } => {
                write!(f, "Connect failed for {endpoint}: {reason}")
            }
            Self::Listening(ep) => write!(f, "Listening on {ep}"),
            Self::Accepted(ep) => write!(f, "Accepted connection from {ep}"),
        }
    }
}

/// Handle for receiving socket events.
///
/// This is a channel receiver that provides a stream of socket lifecycle events.
pub type SocketMonitor = flume::Receiver<SocketEvent>;

/// Internal sender for socket events.
///
/// This is exposed publicly to allow socket implementations to emit events.
pub type SocketEventSender = flume::Sender<SocketEvent>;

/// Creates a new monitoring channel pair.
///
/// This is exposed publicly to allow socket implementations to create monitors.
#[must_use] 
pub fn create_monitor() -> (SocketEventSender, SocketMonitor) {
    flume::unbounded()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn test_socket_event_display() {
        let addr: SocketAddr = "127.0.0.1:5555".parse().unwrap();
        let event = SocketEvent::Connected(Endpoint::Tcp(addr));
        assert_eq!(event.to_string(), "Connected to tcp://127.0.0.1:5555");
    }

    #[test]
    fn test_monitor_channel() {
        let (sender, receiver) = create_monitor();
        let addr: SocketAddr = "127.0.0.1:5555".parse().unwrap();
        sender.send(SocketEvent::Connected(Endpoint::Tcp(addr))).unwrap();
        
        let event = receiver.recv().unwrap();
        assert!(matches!(event, SocketEvent::Connected(_)));
    }
}
