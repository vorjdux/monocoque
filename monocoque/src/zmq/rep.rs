//! REP socket implementation.

use super::common::channel_to_io_error;
use bytes::Bytes;
use compio::net::TcpStream;
use monocoque_core::monitor::{create_monitor, SocketEvent, SocketEventSender, SocketMonitor};
use monocoque_zmtp::rep::RepSocket as InternalRep;
use monocoque_zmtp::SocketType;
use std::io;

/// A REP socket for synchronous reply patterns.
///
/// REP sockets enforce strict alternation between receive and send:
/// - Must call `recv()` to get a request
/// - Must call `send()` to reply before next `recv()`
/// - Automatically handles routing envelopes
///
/// They're used for:
/// - Synchronous RPC servers
/// - Request-reply protocols
/// - Service endpoints
///
/// ## ZeroMQ Compatibility
///
/// Compatible with `zmq::REQ` and `zmq::REP` sockets from libzmq.
///
/// ## Example
///
/// ```rust,no_run
/// use monocoque::zmq::RepSocket;
/// use compio::net::TcpListener;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// // Bind and accept
/// let listener = TcpListener::bind("127.0.0.1:5555").await?;
/// let (stream, _) = listener.accept().await?;
/// let mut socket = RepSocket::from_stream(stream).await?;
///
/// loop {
///     // Receive request
///     if let Some(request) = socket.recv().await {
///         println!("Got request: {:?}", request);
///         
///         // Send reply
///         socket.send(vec![Bytes::from("REPLY")]).await?;
///     }
/// }
/// # }
/// ```
pub struct RepSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalRep<S>,
    monitor: Option<SocketEventSender>,
}

impl RepSocket {
    /// Create a REP socket from an existing TCP stream.


    /// Create a REP socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::from_tcp(stream).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from a TCP stream with custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(Self {
            inner: InternalRep::with_options(stream, config, options).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from any stream with custom options.
    pub async fn with_options<Stream>(
        stream: Stream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<RepSocket<Stream>>
    where
        Stream: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
    {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(RepSocket {
            inner: InternalRep::with_options(stream, config, options).await?,
            monitor: None,
        })
    }
}

// Generic impl - works with any stream type
impl<S> RepSocket<S>
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

    /// Receive a request message.
    ///
    /// This blocks until a request is received. The routing envelope is
    /// automatically extracted and stored for the subsequent `send()` call.
    ///
    /// # Returns
    ///
    /// - `Some(msg)` - Received a request (content only, envelope stripped)
    /// - `None` - Connection closed gracefully or error occurred
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RepSocket;
    ///
    /// # async fn example(socket: &mut RepSocket) -> std::io::Result<()> {
    /// if let Some(request) = socket.recv().await {
    ///     for (i, frame) in request.iter().enumerate() {
    ///         println!("Frame {}: {:?}", i, frame);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> Option<Vec<Bytes>> {
        self.inner.recv().await.ok().flatten()
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub fn socket_type() -> SocketType {
        SocketType::Rep
    }

    /// Get the endpoint this socket is connected/bound to, if available.
    ///
    /// Returns `None` if the socket was created from a raw stream.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub fn last_endpoint(&self) -> Option<&monocoque_core::endpoint::Endpoint> {
        self.inner.last_endpoint()
    }

    /// Check if the last received message has more frames coming.
    ///
    /// Returns `true` if there are more frames in the current multipart message.
    ///\n    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) option.
    #[inline]
    pub fn has_more(&self) -> bool {
        self.inner.has_more()
    }

    /// Get the event state of the socket.
    ///
    /// Returns a bitmask indicating ready-to-receive and ready-to-send states.
    ///
    /// # Returns
    ///
    /// - `1` (POLLIN) - Socket is ready to receive
    /// - `2` (POLLOUT) - Socket is ready to send
    /// - `3` (POLLIN | POLLOUT) - Socket is ready for both
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_EVENTS` (15) option.
    #[inline]
    pub fn events(&self) -> u32 {
        self.inner.events()
    }

    /// Send a reply message.
    ///
    /// This must be called after `recv()` and automatically uses the stored
    /// routing envelope from the request.
    ///
    /// # Arguments
    ///
    /// * `msg` - Vector of message frames (parts) to send as reply
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called without first calling `recv()`
    /// - The underlying connection is closed
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RepSocket;
    /// use bytes::Bytes;
    ///
    /// # async fn example(socket: &mut RepSocket) -> std::io::Result<()> {
    /// // Send single-part reply
    /// socket.send(vec![Bytes::from("OK")]).await?;
    ///
    /// // Send multi-part reply
    /// socket.send(vec![
    ///     Bytes::from("Status: OK"),
    ///     Bytes::from("Data: ..."),
    /// ]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        channel_to_io_error(self.inner.send(msg).await)
    }
}

// Unix-specific impl for IPC support
#[cfg(unix)]
impl RepSocket<compio::net::UnixStream> {
    /// Create a REP socket from an existing Unix domain socket stream (IPC).
    pub async fn from_unix_stream(stream: compio::net::UnixStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::new(stream).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from an existing Unix stream with custom buffer configuration.
    pub async fn from_unix_stream_with_config(
        stream: compio::net::UnixStream,
        config: monocoque_core::config::BufferConfig,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalRep::with_config(stream, config).await?,
            monitor: None,
        })
    }

    /// Create a REP socket from an existing Unix stream with custom options.
    ///
    /// This method provides full control over socket behavior through SocketOptions.
    pub async fn from_unix_stream_with_options(
        stream: compio::net::UnixStream,
        options: monocoque_core::options::SocketOptions,
    ) -> io::Result<Self> {
        let config = monocoque_core::config::BufferConfig {
            read_buf_size: options.read_buffer_size,
            write_buf_size: options.write_buffer_size,
        };
        Ok(Self {
            inner: InternalRep::with_options(stream, config, options).await?,
            monitor: None,
        })
    }
}
