//! Direct-stream DEALER socket implementation
//!
//! This module provides a high-performance DEALER socket using direct stream I/O
//! for minimal latency.
//!
//! # DEALER Pattern
//!
//! DEALER sockets are bidirectional asynchronous sockets that allow sending and
//! receiving messages freely without a strict request-reply pattern.

use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::options::SocketOptions;
use smallvec::SmallVec;
use std::io;
use std::time::Duration;
use tracing::{debug, trace};

use crate::{
    base::SocketBase,
    codec::encode_multipart,
    handshake::perform_handshake_with_timeout,
    session::SocketType,
};
use monocoque_core::config::BufferConfig;
use monocoque_core::endpoint::Endpoint;
use std::fmt;

/// Direct-stream DEALER socket with optional auto-reconnection support.
pub struct DealerSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options, reconnection)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    /// SmallVec avoids heap allocation for 1-4 frame messages (common case)
    frames: SmallVec<[Bytes; 4]>,
}

impl<S> DealerSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new DEALER socket from a stream with large buffer configuration (16KB).
    ///
    /// DEALER sockets typically handle high-throughput workloads with larger messages,
    /// so large buffers provide optimal performance. Use `with_config()` for different workloads.
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, BufferConfig::large(), SocketOptions::default()).await
    }

    /// Create a new DEALER socket from a stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput with large messages
    /// - Use `BufferConfig::custom(read, write)` for fine-grained control
    ///
    /// Works with both TCP and Unix domain sockets.
    ///
    /// **Note**: For TCP streams, use `from_tcp_with_config()` instead to ensure TCP_NODELAY is enabled.
    pub async fn with_config(stream: S, config: BufferConfig) -> io::Result<Self> {
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new DEALER socket with custom buffer configuration and socket options.
    ///
    /// # Arguments
    ///
    /// * `stream` - The underlying stream (TCP or Unix socket)
    /// * `config` - Buffer configuration for read/write operations
    /// * `options` - Socket options (timeouts, limits, etc.)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    /// use monocoque_core::config::BufferConfig;
    /// use monocoque_core::options::SocketOptions;
    /// use std::time::Duration;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let options = SocketOptions::default()
    ///     .with_recv_timeout(Duration::from_secs(5))
    ///     .with_send_timeout(Duration::from_secs(5));
    /// let socket = DealerSocket::with_options(
    ///     stream,
    ///     BufferConfig::large(),
    ///     options
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_options(
        mut stream: S,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[DEALER] Creating new direct DEALER socket");

        // Perform ZMTP handshake with timeout
        debug!("[DEALER] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Dealer,
            None,
            Some(options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[DEALER] Handshake complete"
        );

        debug!("[DEALER] Socket initialized");

        Ok(Self {
            base: SocketBase::new(stream, config, options),
            frames: SmallVec::new(),
        })
    }

    /// Connect to an endpoint with automatic reconnection support.
    ///
    /// This is the recommended way to create a DEALER socket when you want
    /// automatic reconnection on connection failures.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Connection string (e.g., "tcp://127.0.0.1:5555", "ipc:///tmp/socket.sock")
    /// * `config` - Buffer configuration for read/write operations
    /// * `options` - Socket options including reconnection settings
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    /// use monocoque_core::config::BufferConfig;
    /// use monocoque_core::options::SocketOptions;
    /// use std::time::Duration;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let options = SocketOptions::default()
    ///     .with_reconnect_ivl(Duration::from_millis(100))
    ///     .with_reconnect_ivl_max(Duration::from_secs(10));
    ///
    /// let mut socket = DealerSocket::<TcpStream>::connect(
    ///     "tcp://127.0.0.1:5555",
    ///     BufferConfig::large(),
    ///     options
    /// ).await?;
    ///
    /// // Automatically reconnects on failure
    /// socket.send(vec![bytes::Bytes::from("Hello")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(
        endpoint: &str,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self>
    where
        S: From<TcpStream> + fmt::Debug,
    {
        use compio::net::TcpStream;

        debug!("[DEALER] Connecting to endpoint: {}", endpoint);

        // Parse endpoint
        let parsed_endpoint = Endpoint::parse(endpoint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

        // Connect based on endpoint type
        let mut stream = match &parsed_endpoint {
            Endpoint::Tcp(addr) => {
                let tcp_stream = TcpStream::connect(addr).await?;
                S::from(tcp_stream)
            }
            #[cfg(unix)]
            Endpoint::Ipc(path) => {
                use compio::net::UnixStream;
                let _unix_stream = UnixStream::connect(path).await?;
                // This won't work for all S types, but works for TcpStream
                // For Unix streams, user should call with_options directly
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IPC endpoints require explicit UnixStream - use from_unix_stream_with_options()"
                ));
            }
        };

        // Perform handshake
        let handshake_result = perform_handshake_with_timeout(
            &mut stream as &mut S,
            SocketType::Dealer,
            None,
            Some(options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            "[DEALER] Connected to {}", endpoint
        );

        Ok(Self {
            base: SocketBase::with_endpoint(stream, parsed_endpoint, config, options),
            frames: SmallVec::new(),
        })
    }

    /// Receive a message.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[DEALER] Waiting for message");

        // Read from stream until we have a complete message
        loop {
            // Try to decode frames from buffer
            loop {
                match self.base.decoder.decode(&mut self.base.recv)? {
                    Some(frame) => {
                        let more = frame.more();
                        self.frames.push(frame.payload);

                        if !more {
                            // Complete message received
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[DEALER] Received {} frames", msg.len());
                            return Ok(Some(msg));
                        }
                    }
                    None => break, // Need more data
                }
            }

            // Need more data - read raw bytes from stream
            let n = self.base.read_raw().await?;
            if n == 0 {
                // EOF - connection closed
                trace!("[DEALER] Connection closed");
                return Ok(None);
            }
            // Continue decoding with new data
        }
    }

    /// Send a message immediately.
    ///
    /// Encodes and sends the message in a single I/O operation.
    /// For high-throughput scenarios, consider using `send_buffered()` + `flush()`
    /// to batch multiple messages.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[DEALER] Sending {} frames", msg.len());

        // Encode message into write_buf
        self.base.write_buf.clear();
        encode_multipart(&msg, &mut self.base.write_buf);

        // Delegate to base for writing from write_buf
        self.base.write_from_buf().await?;

        trace!("[DEALER] Message sent successfully");
        Ok(())
    }

    /// Send a message to the internal buffer without flushing.
    ///
    /// Use this for batching multiple messages before a single flush.
    /// Call `flush()` to send all buffered messages.
    ///
    /// # High Water Mark (HWM)
    ///
    /// This method enforces the send high water mark (`send_hwm` option).
    /// If the HWM is reached, this returns `io::ErrorKind::WouldBlock`.
    /// The application should either flush pending messages or drop messages
    /// according to the socket pattern requirements.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::DealerSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut socket: DealerSocket) -> std::io::Result<()> {
    /// // Batch 100 messages
    /// for i in 0..100 {
    ///     match socket.send_buffered(vec![Bytes::from(format!("msg {}", i))]) {
    ///         Ok(()) => {},
    ///         Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
    ///             // HWM reached - flush before continuing
    ///             socket.flush().await?;
    ///             socket.send_buffered(vec![Bytes::from(format!("msg {}", i))])?;
    ///         }
    ///         Err(e) => return Err(e),
    ///     }
    /// }
    /// // Single I/O operation for all buffered messages
    /// socket.flush().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_buffered(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        // Check HWM before buffering
        if self.base.buffered_messages >= self.base.options.send_hwm {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "Send high water mark reached ({} messages). Flush or drop messages.",
                    self.base.options.send_hwm
                ),
            ));
        }

        trace!("[DEALER] Buffering {} frames", msg.len());

        // Encode directly into send_buffer
        encode_multipart(&msg, &mut self.base.send_buffer);
        self.base.buffered_messages += 1;
        Ok(())
    }

    /// Flush all buffered messages to the network.
    ///
    /// Sends all messages buffered by `send_buffered()` in a single I/O operation.
    pub async fn flush(&mut self) -> io::Result<()> {
        trace!("[DEALER] Flushing {} bytes", self.base.send_buffer.len());
        self.base.flush_send_buffer().await?;
        trace!("[DEALER] Flush completed");
        Ok(())
    }

    /// Send multiple messages in a single batch (convenience method).
    ///
    /// This is equivalent to calling `send_buffered()` for each message
    /// followed by `flush()`, but more ergonomic.
    ///
    /// # High Water Mark (HWM)
    ///
    /// This method checks HWM for each message. If HWM is reached,
    /// it returns an error. Consider using `send_buffered()` + `flush()`
    /// for more control over HWM handling.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::DealerSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut socket: DealerSocket) -> std::io::Result<()> {
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
        trace!("[DEALER] Batching {} messages", messages.len());

        for msg in messages {
            // Check HWM for each message
            if self.base.buffered_messages >= self.base.options.send_hwm {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "Send high water mark reached ({} messages)",
                        self.base.options.send_hwm
                    ),
                ));
            }
            encode_multipart(msg, &mut self.base.send_buffer);
            self.base.buffered_messages += 1;
        }

        self.flush().await
    }

    /// Get the number of bytes currently buffered.
    #[inline]
    pub fn buffered_bytes(&self) -> usize {
        self.base.send_buffer.len()
    }

    /// Close the socket gracefully, respecting the linger timeout.
    ///
    /// This method attempts to flush any buffered send data before closing.
    /// The behavior depends on the `linger` option:
    ///
    /// - `Some(Duration::ZERO)`: Close immediately, discarding buffered data
    /// - `Some(duration)`: Try to flush buffered data within the timeout
    /// - `None`: Block indefinitely until all data is sent (default libzmq behavior)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::DealerSocket;
    /// # async fn example(mut socket: DealerSocket) -> std::io::Result<()> {
    /// // Send some data
    /// socket.send_buffered(vec![bytes::Bytes::from("data")]);
    ///
    /// // Close gracefully, flushing buffered data
    /// socket.close().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn close(mut self) -> io::Result<()> {
        let linger = self.base.options.linger;
        
        // If no data buffered, just drop the socket
        if self.base.send_buffer.is_empty() {
            trace!("[DEALER] No buffered data, closing immediately");
            return Ok(());
        }

        trace!(
            "[DEALER] Closing with {} bytes buffered, linger={:?}",
            self.base.send_buffer.len(),
            linger
        );

        match linger {
            Some(dur) if dur.is_zero() => {
                // Linger = 0: discard buffered data immediately
                debug!("[DEALER] Linger=0, discarding {} bytes", self.base.send_buffer.len());
                Ok(())
            }
            Some(dur) => {
                // Linger = timeout: try to flush within timeout
                use compio::time::timeout;
                match timeout(dur, self.flush()).await {
                    Ok(Ok(())) => {
                        debug!("[DEALER] Successfully flushed before close");
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        debug!("[DEALER] Flush failed: {}", e);
                        Err(e)
                    }
                    Err(_) => {
                        debug!("[DEALER] Linger timeout expired, closing anyway");
                        // Timeout expired, but we close gracefully anyway
                        Ok(())
                    }
                }
            }
            None => {
                // Linger = indefinite: block until flushed
                debug!("[DEALER] Linger=indefinite, flushing all buffered data");
                self.flush().await
            }
        }
    }

    /// Wait for the appropriate reconnection delay based on socket options.
    ///
    /// This is a convenience method for implementing reconnection logic.
    /// It sleeps for the duration specified in the socket's reconnect options,
    /// providing a simple way to implement exponential backoff.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::DealerSocket;
    /// # use compio::net::TcpStream;
    /// # use std::time::Duration;
    /// # async fn reconnect_loop(addr: &str) -> std::io::Result<()> {
    /// use monocoque_core::reconnect::ReconnectState;
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let options = SocketOptions::default()
    ///     .with_reconnect_ivl(Duration::from_millis(100))
    ///     .with_reconnect_ivl_max(Duration::from_secs(30));
    ///
    /// let mut reconnect = ReconnectState::new(&options);
    ///
    /// loop {
    ///     match TcpStream::connect(addr).await {
    ///         Ok(stream) => {
    ///             let socket = DealerSocket::from_tcp_with_options(
    ///                 stream,
    ///                 monocoque_core::config::BufferConfig::large(),
    ///                 options.clone()
    ///             ).await?;
    ///             reconnect.reset();
    ///             // Use socket...
    ///             break;
    ///         }
    ///         Err(e) => {
    ///             eprintln!("Connection failed: {}, retrying...", e);
    ///             let delay = reconnect.next_delay();
    ///             compio::time::sleep(delay).await;
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_reconnect_delay(options: &SocketOptions, attempt: u32) -> Duration {
        options.next_reconnect_ivl(attempt)
    }

    /// Get a reference to the socket options.
    #[inline]
    pub fn options(&self) -> &SocketOptions {
        &self.base.options
    }

    /// Get a mutable reference to the socket options.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::DealerSocket;
    /// # use std::time::Duration;
    /// # async fn example(mut socket: DealerSocket) {
    /// // Change receive timeout dynamically
    /// socket.options_mut().recv_timeout = Some(Duration::from_secs(10));
    /// # }
    /// ```
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.base.options
    }

    /// Set socket options (builder-style).
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::DealerSocket;
    /// # use monocoque_core::options::SocketOptions;
    /// # use std::time::Duration;
    /// # async fn example(mut socket: DealerSocket) {
    /// socket.set_options(
    ///     SocketOptions::default()
    ///         .with_recv_timeout(Duration::from_secs(5))
    ///         .with_send_timeout(Duration::from_secs(5))
    /// );
    /// # }
    /// ```
    #[inline]
    pub fn set_options(&mut self, options: SocketOptions) {
        self.base.options = options;
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl DealerSocket<TcpStream> {
    /// Create a new DEALER socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_config(stream, BufferConfig::large()).await
    }

    /// Create a new DEALER socket from a TCP stream with TCP_NODELAY and custom config.
    pub async fn from_tcp_with_config(stream: TcpStream, config: BufferConfig) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[DEALER] TCP_NODELAY enabled");
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new DEALER socket from a TCP stream with full configuration.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[DEALER] TCP_NODELAY enabled");
        Self::with_options(stream, config, options).await
    }

    /// Try to reconnect to the stored endpoint.
    ///
    /// Returns Ok(()) if reconnection succeeded, Err otherwise.
    /// On success, resets the poisoned flag and reconnection state.
    async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base.try_reconnect(SocketType::Dealer).await
    }

    /// Receive a message with automatic reconnection.
    ///
    /// If the socket was created with `connect()` and has an endpoint configured,
    /// this will automatically attempt to reconnect on disconnection.
    ///
    /// For sockets created with `from_tcp()`, this behaves the same as `recv()`.
    pub async fn recv_with_reconnect(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // Try to reconnect if disconnected
        if self.base.stream.is_none() {
            trace!("[DEALER] Stream disconnected, attempting reconnection");
            if let Err(e) = self.try_reconnect().await {
                debug!("[DEALER] Reconnection failed: {}", e);
                return Err(e);
            }
        }

        // Now call the regular recv
        self.recv().await
    }

    /// Send a message with automatic reconnection.
    ///
    /// If the socket was created with `connect()` and has an endpoint configured,
    /// this will automatically attempt to reconnect on disconnection.
    ///
    /// For sockets created with `from_tcp()`, this behaves the same as `send()`.
    pub async fn send_with_reconnect(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        // Try to reconnect if disconnected
        if self.base.stream.is_none() {
            trace!("[DEALER] Stream disconnected, attempting reconnection");
            if let Err(e) = self.try_reconnect().await {
                debug!("[DEALER] Reconnection failed: {}", e);
                return Err(e);
            }
        }

        // Now call the regular send
        self.send(msg).await
    }
}
