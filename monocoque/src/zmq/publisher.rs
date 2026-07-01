//! PUB socket implementation with worker pool architecture.

use bytes::Bytes;
use monocoque_core::monitor::{SocketEventSender, SocketMonitor, create_monitor};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::TcpListener;
use monocoque_zmtp::SocketType;
use monocoque_zmtp::publisher::PubSocket as InternalPub;
use std::io;

/// A PUB socket for broadcasting messages to multiple subscribers.
///
/// PubSocket uses a **worker pool architecture** to handle multiple subscribers efficiently:
/// - Multiple OS threads (default: CPU core count)
/// - Each worker runs its own compio runtime with io_uring
/// - Round-robin subscriber distribution across workers
/// - Zero-copy message broadcasting via `Arc<Bytes>`
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
    pub async fn bind(addr: impl monocoque_core::rt::ToSocketAddrs) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            inner: InternalPub::new(),
            listener,
            monitor: None,
        })
    }

    /// Bind with a specific number of worker threads.
    pub async fn bind_with_workers(
        addr: impl monocoque_core::rt::ToSocketAddrs,
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

    /// Broadcast a message given as borrowed frames.
    ///
    /// Allocation-light counterpart to [`send`](Self::send): the shared message
    /// is allocated only when it matches a subscription, so publishing from a
    /// stack array (`send_frames(&[topic, payload])`) pays no per-message heap
    /// allocation on the common drop path of a topic-filtered stream.
    pub async fn send_frames(&mut self, frames: &[Bytes]) -> io::Result<()> {
        self.inner.send_frames(frames).await
    }

    /// Get the number of active subscribers.
    pub const fn subscriber_count(&self) -> usize {
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
    pub const fn socket_type() -> SocketType {
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

    /// Get a mutable reference to this socket's options.
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        self.inner.options_mut()
    }

    /// Number of messages dropped due to HWM backpressure.
    #[inline]
    pub fn drop_count(&self) -> u64 {
        self.inner.drop_count()
    }
}
