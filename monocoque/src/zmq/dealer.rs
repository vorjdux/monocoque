//! DEALER socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_zmtp::dealer::DealerSocket as InternalDealer;
use std::io;

/// A DEALER socket for asynchronous request-reply patterns.
///
/// DEALER sockets are fair-queuing clients that distribute messages
/// across multiple server endpoints. They're used for:
///
/// - Load-balanced request-reply
/// - Async RPC clients
/// - Worker pools
///
/// ## ZeroMQ Compatibility
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
    /// Connect to a ZeroMQ peer and create a DEALER socket.
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
        Self::from_stream(stream).await
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
    pub async fn from_stream(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalDealer::new(stream).await?,
        })
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
        channel_to_io_error(self.inner.send(msg).await)
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
        self.inner.recv().await.ok().flatten()
    }
}
