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
    BindFailed { endpoint: Endpoint, reason: String },

    /// Connection attempt failed.
    ConnectFailed { endpoint: Endpoint, reason: String },

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

/// Bound on the number of undrained monitor events held in the channel.
///
/// Lifecycle events (connect/disconnect/bind/accept) are low volume, but the
/// channel was previously unbounded, so an application that never drained its
/// monitor would let it grow without limit. Bounding it caps that footprint;
/// [`emit`] drops events (rather than blocking the socket path) once full.
pub const MONITOR_CHANNEL_CAP: usize = 256;

/// Creates a new monitoring channel pair.
///
/// The channel is bounded by [`MONITOR_CHANNEL_CAP`]. Emit events with [`emit`],
/// which never blocks the caller.
///
/// This is exposed publicly to allow socket implementations to create monitors.
#[must_use]
pub fn create_monitor() -> (SocketEventSender, SocketMonitor) {
    flume::bounded(MONITOR_CHANNEL_CAP)
}

/// Emit a monitor event without ever blocking the socket path.
///
/// Uses a non-blocking send: if the monitor is full (the application is not
/// draining it) or the receiver has been dropped, the event is discarded rather
/// than stalling the socket operation that produced it.
pub fn emit(sender: &SocketEventSender, event: SocketEvent) {
    let _ = sender.try_send(event);
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
        emit(&sender, SocketEvent::Connected(Endpoint::Tcp(addr)));

        let event = receiver.recv().unwrap();
        assert!(matches!(event, SocketEvent::Connected(_)));
    }

    #[test]
    fn emit_is_bounded_and_never_blocks_when_undrained() {
        // Fill well past the cap without ever draining the receiver. emit must
        // not block or grow the channel beyond MONITOR_CHANNEL_CAP.
        let (sender, receiver) = create_monitor();
        let addr: SocketAddr = "127.0.0.1:5555".parse().unwrap();
        for _ in 0..(MONITOR_CHANNEL_CAP * 4) {
            emit(&sender, SocketEvent::Connected(Endpoint::Tcp(addr)));
        }
        assert_eq!(
            receiver.len(),
            MONITOR_CHANNEL_CAP,
            "monitor channel must stay bounded at its cap when undrained"
        );
    }
}
