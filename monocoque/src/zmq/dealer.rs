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
        let sock = Self::from_tcp(stream).await?;
        sock.emit_event(SocketEvent::Connected(
            monocoque_core::endpoint::Endpoint::Tcp(addr),
        ));
        Ok(sock)
    }

    /// Connect to a ZeroMQ peer with automatic reconnection support.
    ///
    /// This method enables the socket to automatically detect disconnections
    /// and attempt reconnection with exponential backoff. Unlike the basic
    /// `connect()` method, this stores the endpoint and manages the underlying
    /// connection lifecycle.
    ///
    /// Supports TCP endpoints only:
    /// - TCP: `"tcp://127.0.0.1:5555"`
    ///
    /// # Reconnection Behavior
    ///
    /// When a disconnection is detected (EOF, write error, poisoned socket):
    /// - The socket enters a disconnected state
    /// - Next `send()` or `recv()` will attempt reconnection
    /// - Backoff delays: 100ms → 200ms → 400ms → ... (capped at 30s)
    /// - Successful reconnection resets the backoff
    ///
    /// # Arguments
    ///
    /// * `endpoint` - TCP endpoint to connect to (e.g., "tcp://127.0.0.1:5555")
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The initial connection fails
    /// - DNS resolution fails
    /// - Invalid endpoint format
    /// - IPC endpoints (not supported for generic reconnection)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::DealerSocket;
    /// use bytes::Bytes;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // Create socket with reconnection support
    /// let mut socket = DealerSocket::connect_with_reconnect("tcp://127.0.0.1:5555").await?;
    ///
    /// // Send will automatically reconnect on disconnection
    /// loop {
    ///     match socket.send(vec![Bytes::from("REQUEST")]).await {
    ///         Ok(_) => println!("Sent successfully"),
    ///         Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {
    ///             println!("Disconnected, will retry on next send");
    ///         }
    ///         Err(e) => return Err(e.into()),
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Notes
    ///
    /// - For IPC/Unix sockets, use `connect_ipc()` without reconnection
    /// - For explicit stream control without reconnection, use `from_tcp()`
    /// - Reconnection only works for TCP streams
    pub async fn connect_with_reconnect(endpoint: &str) -> io::Result<Self> {
        use monocoque_zmtp::BufferConfig;
        use monocoque_core::options::SocketOptions;

        // Use default config and options
        let config = BufferConfig::default();
        let options = SocketOptions::default();

        let inner = InternalDealer::connect(endpoint, config, options).await?;
        
        // Parse endpoint for monitoring
        let parsed = monocoque_core::endpoint::Endpoint::parse(endpoint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        
        let sock = Self {
            inner,
            monitor: None,
        };
        
        sock.emit_event(SocketEvent::Connected(parsed));
        Ok(sock)
    }

    /// Connect to a ZeroMQ peer with automatic reconnection and custom options.
    ///
    /// Same as `connect_with_reconnect()` but allows customizing socket behavior:
    /// - Buffer sizes (send/recv)
    /// - High water marks (HWM)
    /// - Timeouts
    /// - Identity
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::{DealerSocket, SocketOptions};
    /// use bytes::Bytes;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut socket = DealerSocket::connect_with_reconnect_and_options(
    ///     "tcp://127.0.0.1:5555",
    ///     SocketOptions::default()
    ///         .with_send_hwm(100)
    ///         .with_identity(Some(Bytes::from("worker-1")))
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_with_reconnect_and_options(
        endpoint: &str,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        use monocoque_zmtp::BufferConfig;

        let config = BufferConfig::default();
        let inner = InternalDealer::connect(endpoint, config, options).await?;
        
        // Parse endpoint for monitoring
        let parsed = monocoque_core::endpoint::Endpoint::parse(endpoint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        
        let sock = Self {
            inner,
            monitor: None,
        };
        
        sock.emit_event(SocketEvent::Connected(parsed));
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



    /// Create a DEALER socket from a TCP stream with TCP_NODELAY enabled.
    ///
    /// This method automatically enables TCP_NODELAY for optimal performance,
    /// preventing Nagle's algorithm from buffering small packets.
    ///
    /// Uses default buffer sizes (8KB) and socket options. For custom configuration,
    /// use `with_options()`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::DealerSocket;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = DealerSocket::from_tcp(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalDealer::from_tcp(stream).await?,
            monitor: None,
        })
    }

    /// Create a DEALER socket from a TCP stream with custom options.
    ///
    /// Provides full control over buffer sizes, HWM, timeouts, etc. through SocketOptions.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::{DealerSocket, SocketOptions};
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// 
    /// // Customize HWM only (uses default 8KB buffers)
    /// let socket = DealerSocket::from_tcp_with_options(
    ///     stream,
    ///     SocketOptions::default().with_send_hwm(100)
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::{DealerSocket, SocketOptions};
    /// # use compio::net::TcpStream;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// // Customize both buffers and HWM
    /// let socket = DealerSocket::from_tcp_with_options(
    ///     stream,
    ///     SocketOptions::default()
    ///         .with_buffer_sizes(4096, 4096)  // 4KB buffers for low latency
    ///         .with_send_hwm(100)
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(Self {
            inner: InternalDealer::with_options(stream, config, options).await?,
            monitor: None,
        })
    }

    /// Create a DEALER socket from any stream with custom options.
    ///
    /// This is the most flexible constructor - works with TCP, Unix, or in-memory streams.
    /// Useful for testing with duplex streams.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::{DealerSocket, SocketOptions};
    /// use compio::io::duplex;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (client, _server) = duplex(8192);
    /// let socket = DealerSocket::with_options(
    ///     client,
    ///     SocketOptions::default().with_send_hwm(10)
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_options<Stream>(
        stream: Stream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<DealerSocket<Stream>>
    where
        Stream: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
    {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(DealerSocket {
            inner: InternalDealer::with_options(stream, config, options).await?,
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

    /// Send a message to the internal buffer without flushing.
    ///
    /// Use this for batching multiple messages before a single flush.
    /// Call `flush()` to send all buffered messages.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::DealerSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut socket: DealerSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// // Batch 100 messages
    /// for i in 0..100 {
    ///     socket.send_buffered(vec![Bytes::from(format!("msg {}", i))]).await?;
    /// }
    /// // Single I/O operation for all 100 messages
    /// socket.flush().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_buffered(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        channel_to_io_error(self.inner.send_buffered(msg))
    }

    /// Flush all buffered messages to the network.
    ///
    /// Sends all messages buffered by `send_buffered()` in a single I/O operation.
    pub async fn flush(&mut self) -> io::Result<()> {
        channel_to_io_error(self.inner.flush().await)
    }

    /// Send multiple messages in a single batch (convenience method).
    ///
    /// This is equivalent to calling `send_buffered()` for each message
    /// followed by `flush()`, but more ergonomic.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque::zmq::DealerSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut socket: DealerSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// let messages = vec![
    ///     vec![Bytes::from("msg1")],
    ///     vec![Bytes::from("msg2")],
    ///     vec![Bytes::from("msg3")],
    /// ];
    /// socket.send_batch(&messages).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_batch(&mut self, messages: &[Vec<Bytes>]) -> io::Result<()> {
        channel_to_io_error(self.inner.send_batch(messages).await)
    }

    /// Get the number of bytes currently buffered.
    #[inline]
    pub fn buffered_bytes(&self) -> usize {
        self.inner.buffered_bytes()
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

    /// Create a DEALER socket from an existing Unix stream with custom options.
    pub async fn from_unix_stream_with_options(
        stream: compio::net::UnixStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(Self {
            inner: InternalDealer::with_options(stream, config, options).await?,
            monitor: None,
        })
    }
}
