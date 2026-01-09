//! DEALER socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
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
pub struct DealerSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalDealer<S>,
    monitor: Option<SocketEventSender>,
}

impl DealerSocket {
    /// Connect to a ZeroMQ peer and create a DEALER socket.
    ///
    /// Supports both TCP and IPC endpoints:
    /// - TCP: `"tcp://127.0.0.1:5555"` or `"127.0.0.1:5555"`
    /// - IPC: `"ipc:///tmp/socket.sock"` (Unix only)
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Endpoint to connect to
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The connection fails (network unreachable, connection refused, etc.)
    /// - DNS resolution fails for TCP endpoints
    /// - Invalid endpoint format
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::DealerSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // TCP connection
    /// let socket1 = DealerSocket::connect("tcp://127.0.0.1:5555").await?;
    ///
    /// // IPC connection (Unix only)
    /// #[cfg(unix)]
    /// let socket2 = DealerSocket::connect("ipc:///tmp/socket.sock").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(endpoint: &str) -> io::Result<Self> {
        // Try parsing as endpoint, fall back to raw address
        let addr = if let Ok(monocoque_core::endpoint::Endpoint::Tcp(a)) =
            monocoque_core::endpoint::Endpoint::parse(endpoint)
        {
            a
        } else {
            endpoint
                .parse::<std::net::SocketAddr>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
        };

        let stream = TcpStream::connect(addr).await?;
        let sock = Self::from_stream(stream).await?;
        sock.emit_event(SocketEvent::Connected(
            monocoque_core::endpoint::Endpoint::Tcp(addr),
        ));
        Ok(sock)
    }

    /// Connect to a ZeroMQ peer via IPC (Unix domain sockets).
    ///
    /// Unix-only. Accepts IPC paths with or without `ipc://` prefix.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(unix)]
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use monocoque::zmq::DealerSocket;
    ///
    /// let mut socket = DealerSocket::connect_ipc("/tmp/dealer.sock").await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(unix)]
    pub async fn connect_ipc(path: &str) -> io::Result<DealerSocket<compio::net::UnixStream>> {
        use std::path::PathBuf;

        let clean_path = path.strip_prefix("ipc://").unwrap_or(path);
        let ipc_path = PathBuf::from(clean_path);

        let stream = monocoque_core::ipc::connect(&ipc_path).await?;
        let sock = DealerSocket::from_unix_stream(stream).await?;
        sock.emit_event(SocketEvent::Connected(
            monocoque_core::endpoint::Endpoint::Ipc(ipc_path),
        ));
        Ok(sock)
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
            monitor: None,
        })
    }

    /// Create a DEALER socket from an existing TCP stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput with large messages (recommended)
    pub async fn from_stream_with_config(
        stream: TcpStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalDealer::with_config(stream, config).await?,
            monitor: None,
        })
    }
}

// Generic impl - works with any stream type
impl<S> DealerSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    /// Enable monitoring for this socket.
    ///
    /// Returns a receiver for socket lifecycle events. Once enabled, the socket
    /// will emit events like Connected, Disconnected, etc.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::{DealerSocket, SocketEvent};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
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

// Unix-specific impl for IPC support
#[cfg(unix)]
impl DealerSocket<compio::net::UnixStream> {
    /// Create a DEALER socket from an existing Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalDealer::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a DEALER socket from an existing Unix stream with custom buffer configuration.
    pub async fn from_unix_stream_with_config(
        stream: compio::net::UnixStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalDealer::with_config(stream, config).await?,
            monitor: None,
        })
    }
}
