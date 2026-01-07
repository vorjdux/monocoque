//! PUB socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
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
        channel_to_io_error(self.inner.send(msg).await)
    }
}
