//! `ZeroMQ` protocol implementation.
//!
//! This module provides high-performance ZeroMQ-compatible sockets built on `io_uring`.
//!
//! # Socket Types
//!
//! - [`DealerSocket`] - Asynchronous request-reply client (load-balanced)
//! - [`RouterSocket`] - Identity-based routing server
//! - [`PubSocket`] - Publisher (broadcast to subscribers)
//! - [`SubSocket`] - Subscriber (receive filtered messages)
//!
//! # Quick Start
//!
//! ## DEALER (Client)
//!
//! ```rust,no_run
//! use monocoque::zmq::DealerSocket;
//! use bytes::Bytes;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
//! socket.send(vec![Bytes::from("REQUEST")]).await?;
//!
//! if let Some(reply) = socket.recv().await {
//!     println!("Got reply: {:?}", reply);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## ROUTER (Server)
//!
//! ```rust,no_run
//! use monocoque::zmq::RouterSocket;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let (listener, mut socket) = RouterSocket::bind("127.0.0.1:5555").await?;
//!
//! while let Some(msg) = socket.recv().await {
//!     socket.send(msg).await?; // Echo back
//! }
//! # Ok(())
//! # }
//! ```

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use std::io;

// Re-export internal socket types
use monocoque_zmtp::dealer::DealerSocket as InternalDealer;
use monocoque_zmtp::publisher::PubSocket as InternalPub;
use monocoque_zmtp::router::RouterSocket as InternalRouter;
use monocoque_zmtp::subscriber::SubSocket as InternalSub;

/// A DEALER socket for asynchronous request-reply patterns.
///
/// DEALER sockets are fair-queuing clients that distribute messages
/// across multiple server endpoints. They're used for:
///
/// - Load-balanced request-reply
/// - Async RPC clients
/// - Worker pools
///
/// ## `ZeroMQ` Compatibility
///
/// Compatible with `zmq::DEALER` and `zmq::ROUTER` sockets from libzmq.
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::DealerSocket;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Connect to server
/// let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
///
/// // Send request
/// socket.send(vec![Bytes::from("REQUEST")]).await?;
///
/// // Receive reply
/// if let Some(reply) = socket.recv().await {
///     println!("Got reply: {:?}", reply);
/// }
/// # Ok(())
/// # }
/// ```
pub struct DealerSocket {
    inner: InternalDealer,
}

impl DealerSocket {
    /// Connect to a `ZeroMQ` peer and create a DEALER socket.
    ///
    /// This is the recommended way to create a DEALER socket. It handles
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
    /// use monocoque::zmq::DealerSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let socket = DealerSocket::connect("127.0.0.1:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(addr: impl compio::net::ToSocketAddrsAsync) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self::from_stream(stream).await)
    }

    /// Create a DEALER socket from an existing TCP stream.
    ///
    /// Use this for advanced scenarios where you need full control over
    /// the TCP connection (e.g., custom socket options, TLS wrapping).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::DealerSocket;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// // Configure stream (e.g., set TCP_NODELAY)
    /// let socket = DealerSocket::from_stream(stream).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_stream(stream: TcpStream) -> Self {
        Self {
            inner: InternalDealer::new(stream).await,
        }
    }

    /// Send a multipart message.
    ///
    /// Messages are sent asynchronously - this returns immediately after
    /// queuing the message for transmission.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying connection is closed or broken.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::DealerSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut socket: DealerSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// socket.send(vec![
    ///     Bytes::from("part1"),
    ///     Bytes::from("part2"),
    /// ]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.inner
            .send(msg)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }

    /// Receive a multipart message.
    ///
    /// Returns `None` if the connection is closed.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::DealerSocket;
    /// # async fn example(mut socket: DealerSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// while let Some(msg) = socket.recv().await {
    ///     println!("Received {} parts", msg.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> Option<Vec<Bytes>> {
        self.inner.recv().await.ok()
    }
}

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
/// ## `ZeroMQ` Compatibility
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
        self.inner
            .send(msg)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
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

/// A PUB socket for broadcasting messages.
///
/// PUB sockets broadcast messages to all connected SUB peers.
/// They're used for:
///
/// - Event broadcasting
/// - One-to-many messaging
/// - Topic-based distribution
///
/// ## `ZeroMQ` Compatibility
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
pub struct PubSocket {
    inner: InternalPub,
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
        let socket = Self::from_stream(stream).await;
        Ok((listener, socket))
    }

    /// Create a PUB socket from an existing TCP stream.
    pub async fn from_stream(stream: TcpStream) -> Self {
        Self {
            inner: InternalPub::new(stream).await,
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
        self.inner
            .send(msg)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }
}

/// A SUB socket for receiving filtered messages.
///
/// SUB sockets connect to PUB peers and filter messages by topic prefix.
/// They're used for:
///
/// - Event subscriptions
/// - Topic-based message filtering
/// - Many-to-one aggregation
///
/// ## `ZeroMQ` Compatibility
///
/// Compatible with `zmq::SUB` and `zmq::PUB` sockets from libzmq.
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::SubSocket;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut socket = SubSocket::connect("127.0.0.1:5555").await?;
///
/// // Subscribe to topic
/// socket.subscribe(b"topic").await?;
///
/// // Receive filtered messages
/// while let Some(msg) = socket.recv().await {
///     println!("Received: {:?}", msg);
/// }
/// # Ok(())
/// # }
/// ```
pub struct SubSocket {
    inner: InternalSub,
}

impl SubSocket {
    /// Connect to a PUB peer and create a SUB socket.
    pub async fn connect(addr: impl compio::net::ToSocketAddrsAsync) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self::from_stream(stream).await)
    }

    /// Create a SUB socket from an existing TCP stream.
    pub async fn from_stream(stream: TcpStream) -> Self {
        Self {
            inner: InternalSub::new(stream).await,
        }
    }

    /// Subscribe to messages matching the given topic prefix.
    ///
    /// Empty topic subscribes to all messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the subscription command cannot be sent
    /// (e.g., connection closed).
    pub async fn subscribe(&self, topic: &[u8]) -> io::Result<()> {
        self.inner
            .subscribe(topic)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }

    /// Unsubscribe from messages matching the given topic prefix.
    pub async fn unsubscribe(&self, topic: &[u8]) -> io::Result<()> {
        self.inner
            .unsubscribe(topic)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }

    /// Receive a multipart message.
    ///
    /// Only messages matching subscribed topics will be received.
    /// Returns `None` if the connection is closed.
    pub async fn recv(&mut self) -> Option<Vec<Bytes>> {
        self.inner.recv().await.ok()
    }
}

/// Convenient imports for `ZeroMQ` protocol.
///
/// # Example
///
/// ```rust
/// use monocoque::zmq::prelude::*;
///
/// // Now you have:
/// // - DealerSocket, RouterSocket, PubSocket, SubSocket
/// // - Bytes for zero-copy messages
/// ```
pub mod prelude {
    pub use super::{DealerSocket, PubSocket, RouterSocket, SubSocket};
    pub use bytes::Bytes;
}
