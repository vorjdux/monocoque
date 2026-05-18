//! PULL socket implementation.
//!
//! PULL sockets are used in pipeline patterns for receiving tasks.

use compio::net::{TcpListener, TcpStream};
use monocoque_core::monitor::{create_monitor, SocketEventSender, SocketMonitor};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::PullSocket as InternalPull;
use std::io;

/// PULL socket for receiving tasks in a pipeline.
///
/// PULL sockets receive messages from connected PUSH sockets.
pub struct PullSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalPull<S>,
    monitor: Option<SocketEventSender>,
}

impl PullSocket<TcpStream> {
    /// Bind to `addr`, accept one connection, and return a ready PULL socket.
    ///
    /// Returns the `TcpListener` so the caller can accept further PUSH connections.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::PullSocket;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let (_listener, mut socket) = PullSocket::bind("127.0.0.1:5555").await?;
    /// while let Ok(Some(msg)) = socket.recv().await {
    ///     println!("Got task: {:?}", msg);
    /// }
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

    /// Connect to a PUSH socket at `addr`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::PullSocket;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let mut socket = PullSocket::connect("127.0.0.1:5555").await?;
    /// while let Ok(Some(msg)) = socket.recv().await {
    ///     println!("Got task: {:?}", msg);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(addr: impl compio::net::ToSocketAddrsAsync) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::connect(addr).await?,
            monitor: None,
        })
    }

    /// Connect with custom options, storing the endpoint for automatic reconnection.
    pub async fn connect_with_options(
        addr: impl compio::net::ToSocketAddrsAsync,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::connect_with_options(addr, options).await?,
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

    /// Receive with automatic reconnection on EOF or network error.
    pub async fn recv_with_reconnect(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
        self.inner.recv_with_reconnect().await
    }

    /// Create a PULL socket from a TCP stream.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::from_tcp(stream).await?,
            monitor: None,
        })
    }

    /// Create a PULL socket from a TCP stream with custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::from_tcp_with_options(stream, options).await?,
            monitor: None,
        })
    }
}

impl<S> PullSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    /// Create a PULL socket from any stream.
    pub async fn new(stream: S) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a PULL socket from any stream with custom options.
    pub async fn with_options(stream: S, options: SocketOptions) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::with_options(stream, options).await?,
            monitor: None,
        })
    }

    /// Receive a message.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
        self.inner.recv().await
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
impl PullSocket<compio::net::UnixStream> {
    /// Create a PULL socket from a Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a PULL socket from a Unix domain socket stream with custom options.
    pub async fn from_unix_stream_with_options(
        stream: compio::net::UnixStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::with_options(stream, options).await?,
            monitor: None,
        })
    }
}
