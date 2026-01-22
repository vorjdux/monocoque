//! Message proxy (broker) implementation for ZeroMQ patterns.
//!
//! A proxy connects frontend and backend sockets, forwarding messages
//! bidirectionally. This enables common patterns like message brokers,
//! load balancers, and forwarders without application logic.
//!
//! # Supported Patterns
//!
//! - **PUB-SUB broker**: XSUB frontend ←→ XPUB backend
//! - **REQ-REP load balancer**: ROUTER frontend ←→ DEALER backend
//! - **PUSH-PULL forwarder**: PULL frontend ←→ PUSH backend
//!
//! # Message Flow
//!
//! ```text
//! Publishers → XSUB (frontend) → XPUB (backend) → Subscribers
//! Clients    → ROUTER (frontend) → DEALER (backend) → Workers
//! ```
//!
//! # Example: PUB-SUB Broker
//!
//! ```no_run
//! use monocoque_zmtp::proxy::{proxy, ProxySocket};
//! use monocoque_zmtp::xsub::XSubSocket;
//! use monocoque_zmtp::xpub::XPubSocket;
//!
//! #[compio::main]
//! async fn main() -> std::io::Result<()> {
//!     // Publishers connect to 5555
//!     let mut frontend = XSubSocket::bind("127.0.0.1:5555").await?;
//!     
//!     // Subscribers connect to 5556
//!     let mut backend = XPubSocket::bind("127.0.0.1:5556").await?;
//!     
//!     // Forward messages and subscriptions bidirectionally
//!     proxy(&mut frontend, &mut backend, None).await?;
//!     Ok(())
//! }
//! ```
//!
//! # Example: REQ-REP Load Balancer
//!
//! ```no_run
//! use monocoque_zmtp::proxy::{proxy, ProxySocket};
//! use monocoque_zmtp::router::RouterSocket;
//! use monocoque_zmtp::dealer::DealerSocket;
//!
//! #[compio::main]
//! async fn main() -> std::io::Result<()> {
//!     // Clients connect to 5555
//!     let mut frontend = RouterSocket::bind("127.0.0.1:5555").await?;
//!     
//!     // Workers connect to 5556
//!     let mut backend = DealerSocket::bind("127.0.0.1:5556").await?;
//!     
//!     // Load balance requests across workers
//!     proxy(&mut frontend, &mut backend, None).await?;
//!     Ok(())
//! }
//! ```

use bytes::Bytes;
use std::io;
use tracing::debug;

// Import socket types
use crate::dealer::DealerSocket;
use crate::pair::PairSocket;
use crate::publisher::PubSocket;
use crate::pull::PullSocket;
use crate::push::PushSocket;
use crate::rep::RepSocket;
use crate::req::ReqSocket;
use crate::router::RouterSocket;
use crate::subscriber::SubSocket;
use crate::xpub::XPubSocket;
use crate::xsub::XSubSocket;

/// Socket types that can participate in a proxy.
///
/// Sockets must implement multipart message send/receive operations
/// to be used in a proxy pattern.
///
/// Note: This trait is designed for single-threaded async runtimes like compio
/// and does not require `Send`.
#[async_trait::async_trait(?Send)]
pub trait ProxySocket {
    /// Receive a multipart message from the socket.
    ///
    /// Returns `None` if no message is available or connection closed.
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>>;

    /// Send a multipart message to the socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the send operation fails.
    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()>;

    /// Get a description of the socket for logging.
    fn socket_desc(&self) -> &'static str;
}

/// Run a bidirectional message proxy between frontend and backend sockets.
///
/// Messages are forwarded in both directions:
/// - Frontend → Backend
/// - Backend → Frontend
///
/// An optional capture socket receives copies of all messages for monitoring.
///
/// # Parameters
///
/// - `frontend`: Socket facing clients/publishers
/// - `backend`: Socket facing workers/subscribers
/// - `capture`: Optional socket to receive message copies
///
/// # Patterns
///
/// - **PUB-SUB**: `XSUB` (frontend) ←→ `XPUB` (backend)
/// - **REQ-REP**: `ROUTER` (frontend) ←→ `DEALER` (backend)
/// - **PUSH-PULL**: `PULL` (frontend) ←→ `PUSH` (backend)
///
/// # Blocking
///
/// This function runs forever, forwarding messages until an error occurs.
///
/// # Errors
///
/// Returns an error if a socket operation fails.
///
/// # Example
///
/// ```no_run
/// use monocoque_zmtp::proxy::{proxy, ProxySocket};
/// use monocoque_zmtp::xsub::XSubSocket;
/// use monocoque_zmtp::xpub::XPubSocket;
///
/// #[compio::main]
/// async fn main() -> std::io::Result<()> {
///     let mut frontend = XSubSocket::bind("127.0.0.1:5555").await?;
///     let mut backend = XPubSocket::bind("127.0.0.1:5556").await?;
///     
///     proxy(&mut frontend, &mut backend, None).await
/// }
/// ```
pub async fn proxy<F, B, C>(
    frontend: &mut F,
    backend: &mut B,
    mut capture: Option<&mut C>,
) -> io::Result<()>
where
    F: ProxySocket,
    B: ProxySocket,
    C: ProxySocket,
{
    use futures::{select, FutureExt};
    
    debug!("Starting proxy: {} ←→ {}", frontend.socket_desc(), backend.socket_desc());

    loop {
        // Use select! to multiplex between frontend and backend in single-threaded runtime
        select! {
            // Forward frontend → backend
            msg_result = frontend.recv_multipart().fuse() => {
                if let Some(msg) = msg_result? {
                    debug!("Proxy: {} → {}: {} frames",
                           frontend.socket_desc(),
                           backend.socket_desc(),
                           msg.len());

                    // Send copy to capture if present
                    if let Some(ref mut cap) = capture {
                        if let Err(e) = cap.send_multipart(msg.clone()).await {
                            debug!("Capture socket send failed: {}", e);
                        }
                    }

                    // Forward to backend
                    backend.send_multipart(msg).await?;
                }
            }
            
            // Forward backend → frontend
            msg_result = backend.recv_multipart().fuse() => {
                if let Some(msg) = msg_result? {
                    debug!("Proxy: {} → {}: {} frames",
                           backend.socket_desc(),
                           frontend.socket_desc(),
                           msg.len());

                    // Send copy to capture if present
                    if let Some(ref mut cap) = capture {
                        if let Err(e) = cap.send_multipart(msg.clone()).await {
                            debug!("Capture socket send failed: {}", e);
                        }
                    }

                    // Forward to frontend
                    frontend.send_multipart(msg).await?;
                }
            }
        }
    }
}

// ===== ProxySocket Implementations =====

// XSUB socket (frontend in PUB-SUB broker)
#[async_trait::async_trait(?Send)]
impl ProxySocket for XSubSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, _msg: Vec<Bytes>) -> io::Result<()> {
        // XSUB sends subscriptions, not data messages
        // In a proxy context, we don't forward data back to XSUB
        Ok(())
    }

    fn socket_desc(&self) -> &'static str {
        "XSUB"
    }
}

// XPUB socket (backend in PUB-SUB broker)
#[async_trait::async_trait(?Send)]
impl ProxySocket for XPubSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // XPUB receives subscription events, not data
        // Map subscription events to message format
        if let Some(event) = self.recv_subscription().await? {
            let msg = match event {
                monocoque_core::subscription::SubscriptionEvent::Subscribe(topic) => {
                    vec![Bytes::from(&b"\x01"[..]), topic]
                }
                monocoque_core::subscription::SubscriptionEvent::Unsubscribe(topic) => {
                    vec![Bytes::from(&b"\x00"[..]), topic]
                }
            };
            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "XPUB"
    }
}

// DEALER socket (backend in REQ-REP load balancer)
#[async_trait::async_trait(?Send)]
impl ProxySocket for DealerSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "DEALER"
    }
}

// ROUTER socket (frontend in REQ-REP load balancer)
#[async_trait::async_trait(?Send)]
impl ProxySocket for RouterSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "ROUTER"
    }
}

// PULL socket (frontend in PUSH-PULL forwarder)
#[async_trait::async_trait(?Send)]
impl ProxySocket for PullSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, _msg: Vec<Bytes>) -> io::Result<()> {
        // PULL doesn't send
        Ok(())
    }

    fn socket_desc(&self) -> &'static str {
        "PULL"
    }
}

// PUSH socket (backend in PUSH-PULL forwarder)
#[async_trait::async_trait(?Send)]
impl ProxySocket for PushSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // PUSH doesn't receive
        Ok(None)
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "PUSH"
    }
}

// REQ socket
#[async_trait::async_trait(?Send)]
impl ProxySocket for ReqSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "REQ"
    }
}

// REP socket
#[async_trait::async_trait(?Send)]
impl ProxySocket for RepSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "REP"
    }
}

// PAIR socket
#[async_trait::async_trait(?Send)]
impl ProxySocket for PairSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "PAIR"
    }
}

// PUB socket (typically not used in proxy, but included for completeness)
#[async_trait::async_trait(?Send)]
impl ProxySocket for PubSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // PUB doesn't receive
        Ok(None)
    }

    async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    fn socket_desc(&self) -> &'static str {
        "PUB"
    }
}

// SUB socket (typically not used directly in proxy, XSUB is preferred)
#[async_trait::async_trait(?Send)]
impl ProxySocket for SubSocket {
    async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.recv().await
    }

    async fn send_multipart(&mut self, _msg: Vec<Bytes>) -> io::Result<()> {
        // SUB doesn't send data
        Ok(())
    }

    fn socket_desc(&self) -> &'static str {
        "SUB"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock socket for testing proxy logic
    struct MockSocket {
        name: &'static str,
        recv_queue: Vec<Vec<Bytes>>,
        send_queue: Vec<Vec<Bytes>>,
    }

    impl MockSocket {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                recv_queue: Vec::new(),
                send_queue: Vec::new(),
            }
        }

        fn enqueue(&mut self, msg: Vec<Bytes>) {
            self.recv_queue.push(msg);
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ProxySocket for MockSocket {
        async fn recv_multipart(&mut self) -> io::Result<Option<Vec<Bytes>>> {
            Ok(self.recv_queue.pop())
        }

        async fn send_multipart(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
            self.send_queue.push(msg);
            Ok(())
        }

        fn socket_desc(&self) -> &'static str {
            self.name
        }
    }

    #[test]
    fn test_mock_socket() {
        let mut sock = MockSocket::new("test");
        sock.enqueue(vec![Bytes::from("hello")]);
        assert_eq!(sock.recv_queue.len(), 1);
    }

    // TODO: Add integration tests with real sockets
    // - Test XSUB-XPUB broker pattern
    // - Test ROUTER-DEALER load balancer
    // - Test capture socket monitoring
    // - Test error handling when socket fails
}
