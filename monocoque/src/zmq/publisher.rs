//! PUB socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
use monocoque_zmtp::publisher::PubSocket as InternalPub;
use std::io;

/// A PUB socket for broadcasting messages.
///
/// PUB sockets broadcast messages to all connected SUB peers.
/// They're used for:
///
/// - Event broadcasting
/// - One-to-many messaging
/// - Topic-based distribution
///
/// ## ZeroMQ Compatibility
///
/// Compatible with `zmq::PUB` and `zmq::SUB` sockets from libzmq.
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::PubSocket;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let (listener, mut socket) = PubSocket::bind("127.0.0.1:5555").await?;
///
/// // Broadcast messages
/// socket.send(vec![
///     Bytes::from("topic"),
///     Bytes::from("data"),
/// ]).await?;
/// # Ok(())
/// # }
/// ```
pub struct PubSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalPub<S>,
    monitor: Option<SocketEventSender>,
}

impl PubSocket {
    /// Bind to an address and accept the first subscriber.
    ///
    /// # Returns
    ///
    /// A tuple of `(listener, socket)` where:
    /// - `listener` can be used to accept additional subscribers
    /// - `socket` is ready to broadcast to the first subscriber
    pub async fn bind(
        addr: impl compio::net::ToSocketAddrsAsync,
    ) -> io::Result<(TcpListener, Self)> {
        let listener = TcpListener::bind(addr).await?;
        let (stream, _) = listener.accept().await?;
        let socket = Self::from_stream(stream).await?;
        Ok((listener, socket))
    }

    /// Create a PUB socket from an existing TCP stream.
    pub async fn from_stream(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPub::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a PUB socket from an existing TCP stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency pub/sub with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput pub/sub with large messages (recommended)
    pub async fn from_stream_with_config(
        stream: TcpStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPub::with_config(stream, config).await?,
            monitor: None,
        })
    }
}

// Generic impl - works with any stream type
impl<S> PubSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
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

    /// Broadcast a multipart message to all subscribers.
    ///
    /// The first frame is typically used as a topic for filtering.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying connection is closed or broken.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        channel_to_io_error(self.inner.send(msg).await)
    }
}

// Unix-specific impl for IPC support
#[cfg(unix)]
impl PubSocket<compio::net::UnixStream> {
    /// Create a PUB socket from an existing Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPub::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a PUB socket from an existing Unix stream with custom buffer configuration.
    pub async fn from_unix_stream_with_config(
        stream: compio::net::UnixStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPub::with_config(stream, config).await?,
            monitor: None,
        })
    }
}
