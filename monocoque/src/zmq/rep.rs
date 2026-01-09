//! REP socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
use monocoque_zmtp::rep::RepSocket as InternalRep;
use std::io;

/// A REP socket for synchronous reply patterns.
///
/// REP sockets enforce strict alternation between receive and send:
/// - Must call `recv()` to get a request
/// - Must call `send()` to reply before next `recv()`
/// - Automatically handles routing envelopes
///
/// They're used for:
/// - Synchronous RPC servers
/// - Request-reply protocols
/// - Service endpoints
///
/// ## ZeroMQ Compatibility
///
/// Compatible with `zmq::REQ` and `zmq::REP` sockets from libzmq.
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::RepSocket;
/// use compio::net::TcpListener;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Bind and accept
/// let listener = TcpListener::bind("127.0.0.1:5555").await?;
/// let (stream, _) = listener.accept().await?;
/// let mut socket = RepSocket::from_stream(stream).await?;
///
/// loop {
///     // Receive request
///     if let Some(request) = socket.recv().await {
///         println!("Got request: {:?}", request);
///         
///         // Send reply
///         socket.send(vec![Bytes::from("REPLY")]).await?;
///     }
/// }
/// # }
/// ```
pub struct RepSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalRep<S>,
    monitor: Option<SocketEventSender>,
}

impl RepSocket {
    /// Create a REP socket from an existing TCP stream.
    ///
    /// REP sockets typically accept incoming connections, so this is
    /// used with a listener:
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RepSocket;
    /// use compio::net::TcpListener;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let listener = TcpListener::bind("127.0.0.1:5555").await?;
    /// let (stream, _) = listener.accept().await?;
    /// let socket = RepSocket::from_stream(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_stream(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from an existing TCP stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency request/reply with small messages (recommended)
    /// - Use `BufferConfig::large()` (16KB) for high-throughput with large messages
    pub async fn from_stream_with_config(
        stream: TcpStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::with_config(stream, config).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::from_tcp(stream).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from a TCP stream with TCP_NODELAY and custom config.
    pub async fn from_tcp_with_config(
        stream: TcpStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::from_tcp_with_config(stream, config).await?,
            monitor: None,
        })
    }
}

// Generic impl - works with any stream type
impl<S> RepSocket<S>
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

    /// Receive a request message.
    ///
    /// This blocks until a request is received. The routing envelope is
    /// automatically extracted and stored for the subsequent `send()` call.
    ///
    /// # Returns
    ///
    /// - `Some(msg)` - Received a request (content only, envelope stripped)
    /// - `None` - Connection closed gracefully or error occurred
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RepSocket;
    ///
    /// # async fn example(socket: &mut RepSocket) -> std::io::Result<()> {
    /// if let Some(request) = socket.recv().await {
    ///     for (i, frame) in request.iter().enumerate() {
    ///         println!("Frame {}: {:?}", i, frame);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> Option<Vec<Bytes>> {
        self.inner.recv().await.ok().flatten()
    }

    /// Send a reply message.
    ///
    /// This must be called after `recv()` and automatically uses the stored
    /// routing envelope from the request.
    ///
    /// # Arguments
    ///
    /// * `msg` - Vector of message frames (parts) to send as reply
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called without first calling `recv()`
    /// - The underlying connection is closed
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RepSocket;
    /// use bytes::Bytes;
    ///
    /// # async fn example(socket: &mut RepSocket) -> std::io::Result<()> {
    /// // Send single-part reply
    /// socket.send(vec![Bytes::from("OK")]).await?;
    ///
    /// // Send multi-part reply
    /// socket.send(vec![
    ///     Bytes::from("Status: OK"),
    ///     Bytes::from("Data: ..."),
    /// ]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        channel_to_io_error(self.inner.send(msg).await)
    }
}

// Unix-specific impl for IPC support
#[cfg(unix)]
impl RepSocket<compio::net::UnixStream> {
    /// Create a REP socket from an existing Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from an existing Unix stream with custom buffer configuration.
    pub async fn from_unix_stream_with_config(
        stream: compio::net::UnixStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::with_config(stream, config).await?,
            monitor: None,
        })
    }
}
