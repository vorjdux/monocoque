//! ROUTER socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
use monocoque_zmtp::router::RouterSocket as InternalRouter;
use std::io;

/// A ROUTER socket for identity-based routing.
///
/// ROUTER sockets prefix incoming messages with the sender's identity,
/// and route outgoing messages based on the first frame (identity).
/// They're used for:
///
/// - Async request-reply servers
/// - Brokers and proxies
/// - Stateful connection tracking
///
/// ## ZeroMQ Compatibility
///
/// Compatible with `zmq::ROUTER` and `zmq::DEALER` sockets from libzmq.
///
/// ## Message Format
///
/// **Incoming**: `[identity, delimiter, ...user_frames]`\
/// **Outgoing**: `[identity, delimiter, ...user_frames]` (routes to peer with that identity)
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::RouterSocket;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Bind and accept first connection
/// let (listener, mut socket) = RouterSocket::bind("127.0.0.1:5555").await?;
///
/// // Echo server
/// while let Some(msg) = socket.recv().await {
///     // msg[0] = identity, msg[1] = delimiter, msg[2+] = payload
///     socket.send(msg).await?; // Echo back to sender
/// }
/// # Ok(())
/// # }
/// ```
pub struct RouterSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalRouter<S>,
    monitor: Option<SocketEventSender>,
}

impl RouterSocket {
    /// Bind to an address and accept the first connection.
    ///
    /// This is the recommended way to create a server-side ROUTER socket.
    /// It handles TCP binding, accepting the first connection, and ZMTP handshake.
    ///
    /// # Returns
    ///
    /// A tuple of `(listener, socket)` where:
    /// - `listener` can be used to accept additional connections
    /// - `socket` is ready to send/receive with the first peer
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The address is already in use
    /// - Permission denied (e.g., binding to privileged port without root)
    /// - Invalid address format
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RouterSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (listener, socket) = RouterSocket::bind("127.0.0.1:5555").await?;
    ///
    /// // Use socket for first connection
    /// // Accept more connections from listener if needed:
    /// // let (stream, _) = listener.accept().await?;
    /// // let socket2 = RouterSocket::from_stream(stream).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn bind(
        addr: impl compio::net::ToSocketAddrsAsync,
    ) -> io::Result<(TcpListener, Self)> {
        let listener = TcpListener::bind(addr).await?;
        let (stream, _) = listener.accept().await?;
        let socket = Self::from_tcp(stream).await?;
        Ok((listener, socket))
    }

    /// Create a ROUTER socket from an existing TCP stream.
    ///
    /// **Deprecated**: Use [`RouterSocket::from_tcp()`] instead to enable TCP_NODELAY for optimal latency.
    ///
    /// Use this for advanced scenarios or when accepting multiple connections
    /// from a listener.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RouterSocket;
    /// use compio::net::TcpListener;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let listener = TcpListener::bind("127.0.0.1:5555").await?;
    ///
    /// loop {
    ///     let (stream, addr) = listener.accept().await?;
    ///     println!("New connection from {}", addr);
    ///     // Prefer this:
    ///     let socket = RouterSocket::from_tcp(stream).await?;
    ///     // Over this:
    ///     // let socket = RouterSocket::from_stream(stream).await;
    ///     // Handle socket (e.g., spawn task)
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[deprecated(
        since = "0.1.0",
        note = "Use `from_tcp()` instead to enable TCP_NODELAY"
    )]
    pub async fn from_stream(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRouter::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a ROUTER socket from an existing TCP stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency routing with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput routing with large messages (recommended)
    #[deprecated(
        since = "0.1.0",
        note = "Use `from_tcp_with_config()` instead to enable TCP_NODELAY"
    )]
    pub async fn from_stream_with_config(
        stream: TcpStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRouter::with_config(stream, config).await?,
            monitor: None,
        })
    }

    /// Create a ROUTER socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRouter::from_tcp(stream).await?,
            monitor: None,
        })
    }

    /// Create a ROUTER socket from a TCP stream with custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(Self {
            inner: InternalRouter::with_options(stream, config, options).await?,
            monitor: None,
        })
    }

    /// Create a ROUTER socket from any stream with custom options.
    pub async fn with_options<Stream>(
        stream: Stream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<RouterSocket<Stream>>
    where
        Stream: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
    {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(RouterSocket {
            inner: InternalRouter::with_options(stream, config, options).await?,
            monitor: None,
        })
    }
}

// Generic impl - works with any stream type
impl<S> RouterSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    /// Enable monitoring for this socket.
    ///
    /// Returns a receiver for socket lifecycle events. Once enabled, the socket
    /// will emit events like Accepted, Disconnected, etc.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RouterSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (_listener, mut socket) = RouterSocket::bind("127.0.0.1:5555").await?;
    /// let monitor = socket.monitor();
    ///
    /// // Spawn task to handle events
    /// compio::runtime::spawn(async move {
    ///     while let Ok(event) = monitor.recv_async().await {
    ///         println!("Socket event: {}", event);
    ///     }
    /// });
    /// # Ok(())
    /// # }
    /// ```
    pub fn monitor(&mut self) -> SocketMonitor {
        let (sender, receiver) = create_monitor();
        self.monitor = Some(sender);
        receiver
    }

    /// Helper to emit monitoring events (if monitoring is enabled).
    #[allow(dead_code)]
    fn emit_event(&self, event: SocketEvent) {
        if let Some(monitor) = &self.monitor {
            let _ = monitor.send(event); // Ignore errors if receiver dropped
        }
    }

    /// Send a multipart message.
    ///
    /// The first frame must be the peer identity to route to.
    /// Messages are sent asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying connection is closed or broken.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::RouterSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut socket: RouterSocket, identity: Bytes) -> Result<(), Box<dyn std::error::Error>> {
    /// socket.send(vec![
    ///     identity,              // Route to this peer
    ///     Bytes::new(),          // Delimiter
    ///     Bytes::from("reply"),  // Payload
    /// ]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        channel_to_io_error(self.inner.send(msg).await)
    }

    /// Send a message to the internal buffer without flushing.
    ///
    /// Use this for batching multiple messages before a single flush.
    pub fn send_buffered(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        channel_to_io_error(self.inner.send_buffered(msg))
    }

    /// Flush all buffered messages to the network.
    pub async fn flush(&mut self) -> io::Result<()> {
        channel_to_io_error(self.inner.flush().await)
    }

    /// Send multiple messages in a single batch.
    pub async fn send_batch(&mut self, messages: &[Vec<Bytes>]) -> io::Result<()> {
        channel_to_io_error(self.inner.send_batch(messages).await)
    }

    /// Get the number of bytes currently buffered.
    #[inline]
    pub fn buffered_bytes(&self) -> usize {
        self.inner.buffered_bytes()
    }

    /// Receive a multipart message.
    ///
    /// The returned message will have the sender's identity as the first frame,
    /// followed by a delimiter, then the payload frames.
    ///
    /// Returns `None` if the connection is closed.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::RouterSocket;
    /// # async fn example(mut socket: RouterSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// while let Some(msg) = socket.recv().await {
    ///     let identity = &msg[0];
    ///     let payload = &msg[2..]; // Skip identity and delimiter
    ///     println!("From {:?}: {:?}", identity, payload);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> Option<Vec<Bytes>> {
        self.inner.recv().await.ok().flatten()
    }
}

// Unix-specific impl for IPC support
#[cfg(unix)]
impl RouterSocket<compio::net::UnixStream> {
    /// Create a ROUTER socket from an existing Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRouter::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a ROUTER socket from an existing Unix stream with custom buffer configuration.
    pub async fn from_unix_stream_with_config(
        stream: compio::net::UnixStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRouter::with_config(stream, config).await?,
            monitor: None,
        })
    }

    /// Create a ROUTER socket from an existing Unix stream with custom options.
    ///
    /// This method provides full control over socket behavior through SocketOptions.
    pub async fn from_unix_stream_with_options(
        stream: compio::net::UnixStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(Self {
            inner: InternalRouter::with_options(stream, config, options).await?,
            monitor: None,
        })
    }
}

// Implement ProxySocket for the high-level RouterSocket wrapper
impl monocoque_zmtp::proxy::ProxySocket for RouterSocket<TcpStream> {
    fn recv_multipart<'life0, 'async_trait>(
        &'life0 mut self,
    ) -> ::core::pin::Pin<Box<dyn ::core::future::Future<Output = io::Result<Option<Vec<Bytes>>>> + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { Ok(self.recv().await) })
    }

    fn send_multipart<'life0, 'async_trait>(
        &'life0 mut self,
        msg: Vec<Bytes>,
    ) -> ::core::pin::Pin<Box<dyn ::core::future::Future<Output = io::Result<()>> + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.send(msg).await })
    }

    fn socket_desc(&self) -> &'static str {
        "ROUTER"
    }
}
