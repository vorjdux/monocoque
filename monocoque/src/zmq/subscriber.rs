//! SUB socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_zmtp::subscriber::SubSocket as InternalSub;
use std::io;

/// A SUB socket for receiving filtered messages.
///
/// SUB sockets connect to PUB peers and filter messages by topic prefix.
/// They're used for:
///
/// - Event subscriptions
/// - Topic-based message filtering
/// - Many-to-one aggregation
///
/// ## ZeroMQ Compatibility
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
        channel_to_io_error(self.inner.subscribe(topic).await)
    }

    /// Unsubscribe from messages matching the given topic prefix.
    pub async fn unsubscribe(&self, topic: &[u8]) -> io::Result<()> {
        channel_to_io_error(self.inner.unsubscribe(topic).await)
    }

    /// Receive a multipart message.
    ///
    /// Only messages matching subscribed topics will be received.
    /// Returns `None` if the connection is closed.
    pub async fn recv(&mut self) -> Option<Vec<Bytes>> {
        self.inner.recv().await.ok()
    }
}
