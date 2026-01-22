//! PUB socket implementation with worker pool architecture.

use bytes::Bytes;
use compio::net::TcpListener;
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
use monocoque_zmtp::publisher::PubSocket as InternalPub;
use monocoque_zmtp::SocketType;
use std::io;

/// A PUB socket for broadcasting messages to multiple subscribers.
///
/// PubSocket uses a **worker pool architecture** to handle multiple subscribers efficiently:
/// - Multiple OS threads (default: CPU core count)
/// - Each worker runs its own compio runtime with io_uring
/// - Round-robin subscriber distribution across workers
/// - Zero-copy message broadcasting via Arc<Bytes>
/// - Lock-free subscription management
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::PubSocket;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut socket = PubSocket::bind("127.0.0.1:5555").await?;
///
/// // Accept subscribers (non-blocking with worker pool)
/// socket.accept_subscriber().await?;
///
/// // Broadcast to all subscribers
/// socket.send(vec![Bytes::from("topic"), Bytes::from("data")]).await?;
/// # Ok(())
/// # }
/// ```
pub struct PubSocket {
    inner: InternalPub,
    listener: TcpListener,
    monitor: Option<SocketEventSender>,
}

impl PubSocket {
    /// Bind to an address with default worker count (CPU cores).
    pub async fn bind(addr: impl compio::net::ToSocketAddrsAsync) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            inner: InternalPub::new(),
            listener,
            monitor: None,
        })
    }

    /// Bind with a specific number of worker threads.
    pub async fn bind_with_workers(
        addr: impl compio::net::ToSocketAddrsAsync,
        worker_count: usize,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            inner: InternalPub::with_workers(worker_count),
            listener,
            monitor: None,
        })
    }

    /// Accept a new subscriber connection.
    ///
    /// Performs ZMTP handshake and assigns the subscriber to a worker thread.
    /// Returns the subscriber ID.
    pub async fn accept_subscriber(&mut self) -> io::Result<u64> {
        self.inner.accept_subscriber(&self.listener).await
    }

    /// Broadcast a multipart message to all matching subscribers.
    ///
    /// Messages are distributed to all workers in parallel.
    /// The first frame is typically used as a topic for subscription filtering.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.inner.send(msg).await
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.inner.subscriber_count()
    }

    /// Get the local address this socket is bound to.
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub fn socket_type() -> SocketType {
        SocketType::Pub
    }

    /// Enable monitoring for this socket.
    ///
    /// Returns a receiver for socket lifecycle events.
    pub fn monitor(&mut self) -> SocketMonitor {
        let (sender, receiver) = create_monitor();
        self.monitor = Some(sender);
        receiver
    }

    /// Helper to emit monitoring events (if monitoring is enabled).
    #[allow(dead_code)]
    fn emit_event(&self, event: SocketEvent) {
        if let Some(monitor) = &self.monitor {
            let _ = monitor.send(event);
        }
    }
}
