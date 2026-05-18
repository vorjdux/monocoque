//! PUSH socket implementation.
//!
//! PUSH sockets are used in pipeline patterns for distributing tasks.

use compio::net::{TcpListener, TcpStream};
use monocoque_core::monitor::{create_monitor, SocketEventSender, SocketMonitor};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::PushSocket as InternalPush;
use std::io;

/// PUSH socket for distributing tasks in a pipeline.
///
/// PUSH sockets send messages in a round-robin fashion to connected PULL sockets.
pub struct PushSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalPush<S>,
    monitor: Option<SocketEventSender>,
}

impl PushSocket<TcpStream> {
    /// Bind to `addr`, accept one connection, and return a ready PUSH socket.
    ///
    /// Returns the `TcpListener` so the caller can accept further PULL connections.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::PushSocket;
    /// use bytes::Bytes;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let (_listener, mut socket) = PushSocket::bind("127.0.0.1:5555").await?;
    /// socket.send(vec![Bytes::from("task")]).await?;
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

    /// Connect to a PULL socket at `addr`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::PushSocket;
    /// use bytes::Bytes;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let mut socket = PushSocket::connect("127.0.0.1:5555").await?;
    /// socket.send(vec![Bytes::from("task")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(addr: impl compio::net::ToSocketAddrsAsync) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::connect(addr).await?,
            monitor: None,
        })
    }

    /// Connect with custom options, storing the endpoint for automatic reconnection.
    pub async fn connect_with_options(
        addr: impl compio::net::ToSocketAddrsAsync,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::connect_with_options(addr, options).await?,
            monitor: None,
        })
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    /// Try to reconnect to the stored endpoint.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.inner.try_reconnect().await
    }

    /// Send with automatic reconnection on network error.
    pub async fn send_with_reconnect(&mut self, msg: Vec<bytes::Bytes>) -> io::Result<()> {
        self.inner.send_with_reconnect(msg).await
    }

    /// Create a PUSH socket from a TCP stream.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::from_tcp(stream).await?,
            monitor: None,
        })
    }

    /// Create a PUSH socket from a TCP stream with custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::from_tcp_with_options(stream, options).await?,
            monitor: None,
        })
    }
}

impl<S> PushSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    /// Create a PUSH socket from any stream.
    pub async fn new(stream: S) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a PUSH socket from any stream with custom options.
    pub async fn with_options(stream: S, options: SocketOptions) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::with_options(stream, options).await?,
            monitor: None,
        })
    }

    /// Send a message.
    pub async fn send(&mut self, msg: Vec<bytes::Bytes>) -> io::Result<()> {
        self.inner.send(msg).await
    }

    /// Enable monitoring for this socket.
    pub fn monitor(&mut self) -> SocketMonitor {
        let (sender, receiver) = create_monitor();
        self.monitor = Some(sender);
        receiver
    }

    /// Get a mutable reference to this socket's options.
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        self.inner.options_mut()
    }
}

#[cfg(unix)]
impl PushSocket<compio::net::UnixStream> {
    /// Create a PUSH socket from a Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a PUSH socket from a Unix domain socket stream with custom options.
    pub async fn from_unix_stream_with_options(
        stream: compio::net::UnixStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::with_options(stream, options).await?,
            monitor: None,
        })
    }
}
