//! ROUTER socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
use monocoque_zmtp::router::RouterSocket as InternalRouter;
use monocoque_zmtp::SocketType;
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
    /// - Use `SocketOptions` with `with_buffer_sizes()` instead
    /// - Small buffers (4KB) for low-latency routing with small messages
    /// - Large buffers (16KB) for high-throughput routing with large messages (recommended)
    ///
    ///   Create a ROUTER socket from a TCP stream with TCP_NODELAY enabled.
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
        Ok(Self {
            inner: InternalRouter::from_tcp_with_options(stream, options).await?,
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
        Ok(RouterSocket {
            inner: InternalRouter::with_options(stream, options).await?,
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

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type() -> SocketType {
        SocketType::Router
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

    /// Set the routing identity for the next accepted connection.
    ///
    /// This identity will be used for the next peer that connects to this ROUTER.
    /// The option is consumed after the connection and must be set again for
    /// subsequent connections.
    ///
    /// # Arguments
    ///
    /// * `id` - The identity to assign (1-255 bytes, cannot start with null byte)
    ///
    /// # Errors
    ///
    /// Returns an error if the identity is invalid (empty, too long, or starts
    /// with null byte).
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_CONNECT_ROUTING_ID` (62).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RouterSocket;
    /// use bytes::Bytes;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (_listener, mut router) = RouterSocket::bind("tcp://0.0.0.0:5555").await?;
    /// 
    /// // Assign explicit identity to next connection
    /// router.set_connect_routing_id(b"worker-001".to_vec())?;
    /// 
    /// // When a peer connects, it will be identified as "worker-001"
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_connect_routing_id(&mut self, id: Vec<u8>) -> io::Result<()> {
        // Validate identity for ROUTER socket
        monocoque_core::options::SocketOptions::validate_router_identity(&id)?;
        self.inner.options_mut().connect_routing_id = Some(Bytes::from(id));
        Ok(())
    }

    /// Enable or disable ROUTER_MANDATORY mode.
    ///
    /// When enabled, sending to an unknown identity returns an error.
    /// When disabled (default), messages to unknown identities are silently dropped.
    ///
    /// **Note**: The current single-peer ROUTER implementation doesn't have a
    /// routing table yet, so this option affects future multi-peer support.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_ROUTER_MANDATORY` (33).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::RouterSocket;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (_listener, mut router) = RouterSocket::bind("tcp://0.0.0.0:5555").await?;
    /// 
    /// // Fail fast if routing to unknown peer
    /// router.set_router_mandatory(true);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_router_mandatory(&mut self, enabled: bool) {
        self.inner.options_mut().router_mandatory = enabled;
    }

    /// Enable or disable ROUTER_HANDOVER mode.
    ///
    /// When enabled, a new connection with an existing identity will take over
    /// that identity, closing the old connection.
    /// When disabled (default), duplicate identities are rejected.
    ///
    /// **Note**: The current single-peer ROUTER implementation doesn't have a
    /// routing table yet, so this option affects future multi-peer support.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_ROUTER_HANDOVER` (56).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::RouterSocket;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (_listener, mut router) = RouterSocket::bind("tcp://0.0.0.0:5555").await?;
    /// 
    /// // Allow identity takeover for reconnecting clients
    /// router.set_router_handover(true);
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_router_handover(&mut self, enabled: bool) {
        self.inner.options_mut().router_handover = enabled;
    }

    /// Get the peer identity for this connection.
    ///
    /// Returns the identity of the connected peer. This is either:
    /// - The identity set via `set_connect_routing_id()`
    /// - The peer's self-reported identity from the handshake
    /// - An auto-generated identity
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::RouterSocket;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (_listener, router) = RouterSocket::bind("tcp://0.0.0.0:5555").await?;
    /// 
    /// let identity = router.peer_identity();
    /// println!("Peer identity: {:?}", identity);
    /// # Ok(())
    /// # }
    /// ```
    pub const fn peer_identity(&self) -> &Bytes {
        self.inner.peer_identity()
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

    /// Create a ROUTER socket from an existing Unix stream with custom options.
    ///
    /// This method provides full control over socket behavior through SocketOptions.
    pub async fn from_unix_stream_with_options(
        stream: compio::net::UnixStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRouter::with_options(stream, options).await?,
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
