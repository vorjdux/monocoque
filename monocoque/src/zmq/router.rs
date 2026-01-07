//! ROUTER socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
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
pub struct RouterSocket {
    inner: InternalRouter,
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
        let socket = Self::from_stream(stream).await;
        Ok((listener, socket))
    }

    /// Create a ROUTER socket from an existing TCP stream.
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
    ///     let socket = RouterSocket::from_stream(stream).await;
    ///     // Handle socket (e.g., spawn task)
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_stream(stream: TcpStream) -> Self {
        Self {
            inner: InternalRouter::new(stream).await,
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
        self.inner.recv().await.ok()
    }
}
