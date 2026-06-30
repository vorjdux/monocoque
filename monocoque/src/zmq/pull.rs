//! PULL socket implementation.
//!
//! PULL sockets are used in pipeline patterns for receiving tasks.

use monocoque_core::monitor::{SocketEventSender, SocketMonitor, create_monitor};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{TcpListener, TcpStream};
use monocoque_zmtp::PullSocket as InternalPull;
use std::io;

/// PULL socket for receiving tasks in a pipeline.
///
/// PULL sockets receive messages from connected PUSH sockets.
pub struct PullSocket<S = TcpStream>
where
    S: compio_io::AsyncRead + compio_io::AsyncWrite + Unpin,
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
        addr: impl monocoque_core::rt::ToSocketAddrs,
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
    pub async fn connect(addr: impl monocoque_core::rt::ToSocketAddrs) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::connect(addr).await?,
            monitor: None,
        })
    }

    /// Connect with custom options, storing the endpoint for automatic reconnection.
    pub async fn connect_with_options(
        addr: impl monocoque_core::rt::ToSocketAddrs,
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
    S: compio_io::AsyncRead + compio_io::AsyncWrite + Unpin,
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

    /// Try to receive a message from the already-buffered input without a kernel read.
    ///
    /// Returns `Ok(None)` immediately when the receive buffer is empty. Use
    /// after `recv()` to drain all messages delivered in one kernel read before
    /// going back to the event loop. This reduces io_uring submissions for
    /// throughput-bound pull loops.
    ///
    /// ```rust,no_run
    /// # async fn example(pull: &mut monocoque::zmq::PullSocket) -> std::io::Result<()> {
    /// if let Some(first) = pull.recv().await? {
    ///     drop(first);
    ///     while let Some(msg) = pull.try_recv()? {
    ///         drop(msg);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_recv(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
        self.inner.try_recv()
    }

    /// Receive a message.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
        self.inner.recv().await
    }

    /// Receive a message into a caller-provided buffer, reusing its allocation.
    ///
    /// Like [`recv`](Self::recv) but the frames are written into `out` (cleared
    /// first) instead of a freshly allocated `Vec`. Passing the same `out` on
    /// every call removes the per-message allocation from a steady recv loop,
    /// which is the dominant per-message cost for small messages. Returns
    /// `Ok(true)` when a message was read, `Ok(false)` when the connection closed.
    pub async fn recv_into(&mut self, out: &mut Vec<bytes::Bytes>) -> io::Result<bool> {
        self.inner.recv_into(out).await
    }

    /// Try to receive a message into a caller-provided buffer without a kernel read.
    ///
    /// The allocation-free counterpart to [`try_recv`](Self::try_recv): returns
    /// `Ok(true)` with the frames moved into `out` (reusing its capacity) when a
    /// complete message is already buffered, or `Ok(false)` leaving `out` untouched
    /// when none is. Use it with [`recv_into`](Self::recv_into) to drain a burst
    /// from one kernel read without allocating per message.
    pub fn try_recv_into(&mut self, out: &mut Vec<bytes::Bytes>) -> io::Result<bool> {
        self.inner.try_recv_into(out)
    }

    /// Receive a batch of messages with a single `.await`.
    ///
    /// Blocks until at least one message is available, then drains every further
    /// message already decoded from the same kernel read. Returning a burst of
    /// small messages from one `.await` amortizes per-await overhead; it is the
    /// receive-side counterpart to [`PushSocket::send_batch`](crate::zmq::PushSocket::send_batch).
    pub async fn recv_batch(&mut self) -> io::Result<Option<Vec<Vec<bytes::Bytes>>>> {
        self.inner.recv_batch().await
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
impl PullSocket<monocoque_core::rt::UnixStream> {
    /// Create a PULL socket from a Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: monocoque_core::rt::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a PULL socket from a Unix domain socket stream with custom options.
    pub async fn from_unix_stream_with_options(
        stream: monocoque_core::rt::UnixStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::with_options(stream, options).await?,
            monitor: None,
        })
    }
}
