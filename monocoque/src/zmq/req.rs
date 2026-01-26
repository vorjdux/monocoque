//! REQ socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
use monocoque_zmtp::req::ReqSocket as InternalReq;
use monocoque_zmtp::SocketType;
use std::io;

/// A REQ socket for synchronous request-reply patterns.
///
/// REQ sockets enforce strict alternation between send and receive:
/// - Must call `send()` before `recv()`
/// - Must call `recv()` before next `send()`
///
/// They're used for:
/// - Synchronous RPC clients
/// - Request-reply protocols
/// - Client-server communication
///
/// ## ZeroMQ Compatibility
///
/// Compatible with `zmq::REQ` and `zmq::REP` sockets from libzmq.
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::ReqSocket;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Connect to server
/// let mut socket = ReqSocket::connect("127.0.0.1:5555").await?;
///
/// // Send request
/// socket.send(vec![Bytes::from("REQUEST")]).await?;
///
/// // Must receive before next send
/// if let Some(reply) = socket.recv().await {
///     println!("Got reply: {:?}", reply);
/// }
///
/// // Now can send again
/// socket.send(vec![Bytes::from("ANOTHER")]).await?;
/// if let Some(reply) = socket.recv().await {
///     println!("Got reply: {:?}", reply);
/// }
/// # Ok(())
/// # }
/// ```
pub struct ReqSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalReq<S>,
    monitor: Option<SocketEventSender>,
}

impl ReqSocket {
    /// Connect to a ZeroMQ peer and create a REQ socket.
    ///
    /// Supports both TCP and IPC endpoints:
    /// - TCP: `"tcp://127.0.0.1:5555"` or `"127.0.0.1:5555"`
    /// - IPC: `"ipc:///tmp/socket.sock"` (Unix only)
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Endpoint to connect to
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or handshake fails.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let socket = ReqSocket::connect("tcp://127.0.0.1:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(endpoint: &str) -> io::Result<Self> {
        // Try parsing as endpoint, fall back to raw address
        let addr = if let Ok(monocoque_core::endpoint::Endpoint::Tcp(a)) =
            monocoque_core::endpoint::Endpoint::parse(endpoint)
        {
            a
        } else {
            endpoint
                .parse::<std::net::SocketAddr>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
        };

        let stream = TcpStream::connect(addr).await?;
        let sock = Self::from_tcp(stream).await?;
        sock.emit_event(SocketEvent::Connected(
            monocoque_core::endpoint::Endpoint::Tcp(addr),
        ));
        Ok(sock)
    }

    /// Connect to a ZeroMQ peer with custom socket options.
    ///
    /// This allows configuring timeouts and other options before connection.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    /// use monocoque::SocketOptions;
    /// use std::time::Duration;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let options = SocketOptions::default()
    ///     .with_send_timeout(Duration::from_secs(5))
    ///     .with_recv_timeout(Duration::from_secs(10));
    ///
    /// let socket = ReqSocket::connect_with_options(
    ///     "tcp://127.0.0.1:5555",
    ///     options
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_with_options(
        endpoint: &str,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        let addr = if let Ok(monocoque_core::endpoint::Endpoint::Tcp(a)) =
            monocoque_core::endpoint::Endpoint::parse(endpoint)
        {
            a
        } else {
            endpoint
                .parse::<std::net::SocketAddr>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
        };

        let stream = TcpStream::connect(addr).await?;
        let sock = Self::from_tcp_with_options(stream, options).await?;
        sock.emit_event(SocketEvent::Connected(
            monocoque_core::endpoint::Endpoint::Tcp(addr),
        ));
        Ok(sock)
    }

    /// Connect to a ZeroMQ peer via IPC (Unix domain sockets).
    ///
    /// Unix-only. Accepts IPC paths with or without `ipc://` prefix.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(unix)]
    /// # async fn example() -> std::io::Result<()> {
    /// use monocoque::zmq::ReqSocket;
    ///
    /// let socket = ReqSocket::connect_ipc("/tmp/req.sock").await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(unix)]
    pub async fn connect_ipc(path: &str) -> io::Result<ReqSocket<compio::net::UnixStream>> {
        use std::path::PathBuf;

        let clean_path = path.strip_prefix("ipc://").unwrap_or(path);
        let ipc_path = PathBuf::from(clean_path);

        let stream = monocoque_core::ipc::connect(&ipc_path).await?;
        let sock = ReqSocket::from_unix_stream(stream).await?;
        sock.emit_event(SocketEvent::Connected(
            monocoque_core::endpoint::Endpoint::Ipc(ipc_path),
        ));
        Ok(sock)
    }



    /// Create a REQ socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalReq::from_tcp(stream).await?,
            monitor: None,
        })
    }

    /// Create a REQ socket from a TCP stream with custom socket options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalReq::from_tcp_with_options(stream, options).await?,
            monitor: None,
        })
    }

    /// Create a REQ socket from any stream with custom options.
    pub async fn with_options<Stream>(
        stream: Stream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<ReqSocket<Stream>>
    where
        Stream: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
    {
        Ok(ReqSocket {
            inner: InternalReq::with_options(stream, options).await?,
            monitor: None,
        })
    }
}

// Generic impl - works with any stream type
impl<S> ReqSocket<S>
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

    /// Send a multipart message.
    ///
    /// This enforces the REQ state machine - you must call `recv()` before
    /// calling `send()` again.
    ///
    /// # Arguments
    ///
    /// * `msg` - Vector of message frames (parts)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called while awaiting a reply (must call `recv()` first)
    /// - The underlying connection is closed
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    /// use bytes::Bytes;
    ///
    /// # async fn example(socket: &mut ReqSocket) -> std::io::Result<()> {
    /// // Send single-part message
    /// socket.send(vec![Bytes::from("Hello")]).await?;
    ///
    /// // Send multi-part message
    /// socket.send(vec![
    ///     Bytes::from("Part 1"),
    ///     Bytes::from("Part 2"),
    /// ]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        channel_to_io_error(self.inner.send(msg).await)
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type() -> SocketType {
        SocketType::Req
    }

    /// Get the endpoint this socket is connected/bound to, if available.
    ///
    /// Returns `None` if the socket was created from a raw stream.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub fn last_endpoint(&self) -> Option<&monocoque_core::endpoint::Endpoint> {
        self.inner.last_endpoint()
    }

    /// Check if the last received message has more frames coming.
    ///
    /// Returns `true` if there are more frames in the current multipart message.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) option.
    #[inline]
    pub fn has_more(&self) -> bool {
        self.inner.has_more()
    }

    /// Get the event state of the socket.
    ///
    /// Returns a bitmask indicating ready-to-receive and ready-to-send states.
    ///
    /// # Returns
    ///
    /// - `1` (POLLIN) - Socket is ready to receive
    /// - `2` (POLLOUT) - Socket is ready to send
    /// - `3` (POLLIN | POLLOUT) - Socket is ready for both
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_EVENTS` (15) option.
    #[inline]
    pub fn events(&self) -> u32 {
        self.inner.events()
    }

    /// Receive a multipart message.
    ///
    /// This blocks until a reply is received. You must call this after `send()`
    /// before calling `send()` again.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(msg))` - Received a multipart message
    /// - `Ok(None)` - Connection closed gracefully
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying channel fails.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    ///
    /// # async fn example(socket: &mut ReqSocket) -> std::io::Result<()> {
    /// if let Some(reply) = socket.recv().await {
    ///     for (i, frame) in reply.iter().enumerate() {
    ///         println!("Frame {}: {:?}", i, frame);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> Option<Vec<Bytes>> {
        self.inner.recv().await.ok().flatten()
    }

    /// Get a reference to the socket options.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    ///
    /// # async fn example(socket: &ReqSocket) {
    /// let timeout = socket.options().recv_timeout;
    /// println!("Receive timeout: {:?}", timeout);
    /// # }
    /// ```
    pub const fn options(&self) -> &monocoque_core::options::SocketOptions {
        self.inner.options()
    }

    /// Get a mutable reference to the socket options.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    /// use std::time::Duration;
    ///
    /// # async fn example(socket: &mut ReqSocket) {
    /// socket.options_mut().recv_timeout = Some(Duration::from_secs(30));
    /// # }
    /// ```
    pub fn options_mut(&mut self) -> &mut monocoque_core::options::SocketOptions {
        self.inner.options_mut()
    }

    /// Set the socket options.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    /// use monocoque::SocketOptions;
    /// use std::time::Duration;
    ///
    /// # async fn example(socket: &mut ReqSocket) {
    /// let options = SocketOptions::default()
    ///     .with_send_timeout(Duration::from_secs(5))
    ///     .with_recv_timeout(Duration::from_secs(10));
    ///
    /// socket.set_options(options);
    /// # }
    /// ```
    pub fn set_options(&mut self, options: monocoque_core::options::SocketOptions) {
        self.inner.set_options(options);
    }
}

// Unix-specific impl for IPC support
#[cfg(unix)]
impl ReqSocket<compio::net::UnixStream> {
    /// Create a REQ socket from an existing Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalReq::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a REQ socket from an existing Unix stream with custom options.
    ///
    /// This method provides full control over socket behavior through SocketOptions.
    pub async fn from_unix_stream_with_options(
        stream: compio::net::UnixStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalReq::with_options(stream, options).await?,
            monitor: None,
        })
    }
}
