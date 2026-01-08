//! REQ socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_zmtp::req::ReqSocket as InternalReq;
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
/// let socket = ReqSocket::connect("127.0.0.1:5555").await?;
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
/// let reply = socket.recv().await;
/// # Ok(())
/// # }
/// ```
pub struct ReqSocket {
    inner: InternalReq,
}

impl ReqSocket {
    /// Connect to a ZeroMQ peer and create a REQ socket.
    ///
    /// This is the recommended way to create a REQ socket. It handles
    /// TCP connection and ZMTP handshake automatically.
    ///
    /// # Arguments
    ///
    /// * `addr` - Socket address to connect to (e.g., `"127.0.0.1:5555"`)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The TCP connection fails (network unreachable, connection refused, etc.)
    /// - DNS resolution fails for the provided address
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let socket = ReqSocket::connect("127.0.0.1:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(addr: impl AsRef<str>) -> io::Result<Self> {
        let stream = TcpStream::connect(addr.as_ref()).await?;
        Self::from_stream(stream).await
    }

    /// Create a REQ socket from an existing TCP stream.
    ///
    /// Use this when you need more control over the TCP connection,
    /// such as setting socket options or using a pre-connected stream.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::ReqSocket;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = ReqSocket::from_stream(stream).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_stream(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalReq::new(stream).await?,
        })
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
    /// # async fn example(socket: &ReqSocket) -> std::io::Result<()> {
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
    /// # async fn example(socket: &ReqSocket) -> std::io::Result<()> {
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
}
