//! SUB socket implementation.

use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
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
/// socket.subscribe(b"topic");
///
/// // Receive filtered messages
/// loop {
///     match socket.recv().await? {
///         Some(msg) => println!("Received: {:?}", msg),
///         None => break, // Connection closed
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct SubSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalSub<S>,
    monitor: Option<SocketEventSender>,
}

impl SubSocket {
    /// Connect to a PUB peer and create a SUB socket.
    ///
    /// Accepts TCP endpoints or raw socket addresses:
    /// - `"tcp://127.0.0.1:5555"`
    /// - `"127.0.0.1:5555"`
    ///
    /// For IPC (Unix domain sockets), use [`SubSocket::connect_ipc()`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::SubSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut socket = SubSocket::connect("127.0.0.1:5555").await?;
    /// socket.subscribe(b""); // Subscribe to all messages
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(endpoint: &str) -> io::Result<Self> {
        // Try parsing as endpoint, fall back to raw address
        let addr = if let Ok(monocoque_core::endpoint::Endpoint::Tcp(a)) = 
            monocoque_core::endpoint::Endpoint::parse(endpoint) {
            a
        } else {
            endpoint.parse::<std::net::SocketAddr>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
        };

        let stream = TcpStream::connect(addr).await?;
        let sock = Self::from_stream(stream).await?;
        sock.emit_event(SocketEvent::Connected(monocoque_core::endpoint::Endpoint::Tcp(addr)));
        Ok(sock)
    }

    /// Connect to a PUB peer via IPC (Unix domain sockets).
    ///
    /// Unix-only. Accepts IPC paths with or without `ipc://` prefix:
    /// - `"ipc:///tmp/socket.sock"`
    /// - `"/tmp/socket.sock"`
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(unix)]
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use monocoque::zmq::SubSocket;
    ///
    /// let mut socket = SubSocket::connect_ipc("/tmp/pubsub.sock").await?;
    /// socket.subscribe(b"");
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(unix)]
    pub async fn connect_ipc(path: &str) -> io::Result<SubSocket<compio::net::UnixStream>> {
        use std::path::PathBuf;
        
        // Strip "ipc://" prefix if present
        let clean_path = path.strip_prefix("ipc://").unwrap_or(path);
        let ipc_path = PathBuf::from(clean_path);

        let stream = monocoque_core::ipc::connect(&ipc_path).await?;
        let sock = SubSocket::from_unix_stream(stream).await?;
        sock.emit_event(SocketEvent::Connected(monocoque_core::endpoint::Endpoint::Ipc(ipc_path)));
        Ok(sock)
    }

    /// Create a SUB socket from an existing TCP stream.
    pub async fn from_stream(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalSub::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a SUB socket from an existing TCP stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency pub/sub with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput pub/sub with large messages (recommended)
    pub async fn from_stream_with_config(
        stream: TcpStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalSub::with_config(stream, config).await?,
            monitor: None,
        })
    }
}

// Generic impl - works with any stream type
impl<S> SubSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    /// Enable monitoring for this socket.
    ///
    /// Returns a receiver for socket lifecycle events.
    pub fn monitor(&mut self) -> SocketMonitor {
        let (sender, receiver) = create_monitor();
        self.monitor = Some(sender);
        receiver
    }

    /// Helper to emit monitoring events (if monitoring is enabled).
    #[allow(dead_code)]
    fn emit_event(&self, event: SocketEvent) {
        if let Some(monitor) = &self.monitor {
            let _ = monitor.send(event);
        }
    }

    /// Subscribe to messages matching the given topic prefix.
    ///
    /// Empty topic subscribes to all messages.
    pub fn subscribe(&mut self, topic: &[u8]) {
        self.inner.subscribe(Bytes::copy_from_slice(topic));
    }

    /// Unsubscribe from messages matching the given topic prefix.
    pub fn unsubscribe(&mut self, topic: &[u8]) {
        self.inner.unsubscribe(&Bytes::copy_from_slice(topic));
    }

    /// Receive a multipart message.
    ///
    /// Only messages matching subscribed topics will be received.
    /// Returns `None` if the connection is closed.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        self.inner.recv().await
    }
}

// Unix-specific impl for IPC support
#[cfg(unix)]
impl SubSocket<compio::net::UnixStream> {
    /// Create a SUB socket from an existing Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalSub::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a SUB socket from an existing Unix stream with custom buffer configuration.
    pub async fn from_unix_stream_with_config(
        stream: compio::net::UnixStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalSub::with_config(stream, config).await?,
            monitor: None,
        })
    }
}
