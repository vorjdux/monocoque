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

use crate::{base::SocketBase, handshake::perform_handshake_with_options, session::SocketType};
use monocoque_core::endpoint::Endpoint;

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
    /// Create a new DEALER socket from a stream with default options.
    ///
    /// Uses default buffer sizes (8KB) optimized for balanced workloads.
    /// For high-throughput, use `SocketOptions::large()`.
    ///
    /// Works with both TCP and Unix domain sockets.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = DealerSocket::new(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new DEALER socket with custom options.
    ///
    /// This provides full control over buffer sizes, timeouts, and all socket options.
    /// Follows the MongoDB Rust driver pattern for ergonomic configuration.
    ///
    /// # Arguments
    ///
    /// * `stream` - The underlying stream (TCP or Unix socket)
    /// * `options` - Socket options (buffers, timeouts, limits, etc.)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    /// use monocoque_core::options::SocketOptions;
    /// use std::time::Duration;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    ///
    /// // High-throughput configuration
    /// let options = SocketOptions::large()
    ///     .with_recv_timeout(Duration::from_secs(5))
    ///     .with_send_timeout(Duration::from_secs(5));
    ///     
    /// let socket = DealerSocket::with_options(stream, options).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[DEALER] Creating new direct DEALER socket");

        // Perform ZMTP handshake with timeout
        debug!("[DEALER] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Dealer,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[DEALER] Handshake complete"
        );

        debug!("[DEALER] Socket initialized");

        let mut base = SocketBase::new(stream, SocketType::Dealer, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
        })
    }

    /// Connect to an endpoint with automatic reconnection support.

    /// Receive a message.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[DEALER] Waiting for message");

        // Read from stream until we have a complete message
        loop {
            // Try to decode frames from buffer
            loop {
                match self.base.process_frame()? {
                    crate::base::FrameResult::NeedMore => break,
                    crate::base::FrameResult::CommandHandled => {
                        if !self.base.send_buffer.is_empty() {
                            self.base.flush_send_buffer().await?;
                        }
                    }
                    crate::base::FrameResult::Data(more, payload) => {
                        self.frames.push(payload);
                        if !more {
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[DEALER] Received {} frames", msg.len());
                            return Ok(Some(msg));
                        }
                    }
                }
            }

            // Need more data - read raw bytes from stream
            let n = self.base.read_raw().await?;
            if n == 0 {
                // EOF - connection closed
                trace!("[DEALER] Connection closed");
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
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

        // Encode message into write_buf (with CURVE encryption if active)
        self.base.encode_message_to_write_buf(&msg)?;

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

        // Encode directly into send_buffer (with CURVE encryption if active)
        self.base.encode_message_to_send_buf(&msg)?;
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
            self.base.encode_message_to_send_buf(msg)?;
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
                debug!(
                    "[DEALER] Linger=0, discarding {} bytes",
                    self.base.send_buffer.len()
                );
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
    /// ```rust,ignore
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
    pub const fn options(&self) -> &SocketOptions {
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

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type(&self) -> SocketType {
        SocketType::Dealer
    }

    /// Get the endpoint this socket is connected/bound to, if available.
    ///
    /// Returns `None` if the socket was created from a raw stream.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub fn last_endpoint(&self) -> Option<&Endpoint> {
        self.base.last_endpoint()
    }

    /// Get the endpoint as a string, if available.
    #[inline]
    pub fn last_endpoint_string(&self) -> Option<&str> {
        self.base.last_endpoint_string()
    }

    /// Check if the last received message has more frames coming.
    ///
    /// Returns `true` if there are more frames in the current multipart message.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) option.
    #[inline]
    pub fn has_more(&self) -> bool {
        self.base.has_more()
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
        self.base.events()
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl DealerSocket<TcpStream> {
    /// Bind to an address and accept the first connection.
    ///
    /// Creates a TCP listener and waits for the first incoming connection.
    /// Returns both the listener (for accepting additional connections) and
    /// the socket connected to the first peer.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    /// use monocoque_core::options::SocketOptions;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let (listener, mut socket) = DealerSocket::bind("127.0.0.1:5555").await?;
    ///
    /// // Use the socket for first connection
    /// let msg = socket.recv().await?;
    ///
    /// // Accept additional connections if needed
    /// let (stream, _) = listener.accept().await?;
    /// let socket2 = DealerSocket::from_tcp(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn bind(
        addr: impl compio::net::ToSocketAddrsAsync,
    ) -> io::Result<(compio::net::TcpListener, Self)> {
        let listener = compio::net::TcpListener::bind(addr).await?;
        let (stream, _) = listener.accept().await?;
        let socket = Self::from_tcp(stream).await?;
        Ok((listener, socket))
    }

    /// Connect to a remote DEALER socket, storing the endpoint for automatic reconnection.
    ///
    /// Unlike `from_tcp()`, this method stores the remote address so that
    /// `recv_with_reconnect()` and `send_with_reconnect()` can re-establish
    /// the connection after a server restart or network hiccup.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
    /// socket.send(vec![bytes::Bytes::from("Hello")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(addr: impl compio::net::ToSocketAddrsAsync) -> io::Result<Self> {
        Self::connect_with_options(addr, SocketOptions::default()).await
    }

    /// Connect with custom options, storing the endpoint for reconnection.
    pub async fn connect_with_options(
        addr: impl compio::net::ToSocketAddrsAsync,
        options: SocketOptions,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        let peer_addr = stream.peer_addr()?;
        crate::utils::configure_tcp_stream(&stream, &options, "DEALER")?;

        let mut stream = stream;
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Dealer,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[DEALER] Connected to {} (endpoint stored for reconnection)",
            peer_addr
        );

        let endpoint = monocoque_core::endpoint::Endpoint::Tcp(peer_addr);
        let mut base =
            crate::base::SocketBase::with_endpoint(stream, SocketType::Dealer, endpoint, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: smallvec::SmallVec::new(),
        })
    }

    /// Create a new DEALER socket from a TCP stream with default options.
    ///
    /// Automatically enables TCP_NODELAY for low latency and applies
    /// TCP keepalive if configured in options.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = DealerSocket::from_tcp(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a new DEALER socket from a TCP stream with custom options.
    ///
    /// Automatically enables TCP_NODELAY and applies TCP keepalive settings.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::DealerSocket;
    /// use monocoque_core::options::SocketOptions;
    /// use compio::net::TcpStream;
    /// use std::time::Duration;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    ///
    /// let options = SocketOptions::large()  // 16KB buffers
    ///     .with_tcp_keepalive(1)
    ///     .with_tcp_keepalive_idle(60)
    ///     .with_recv_timeout(Duration::from_secs(5));
    ///     
    /// let socket = DealerSocket::from_tcp_with_options(stream, options).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Configure TCP optimizations
        crate::utils::configure_tcp_stream(&stream, &options, "DEALER")?;
        Self::with_options(stream, options).await
    }

    /// Try to reconnect to the stored endpoint.
    ///
    /// Returns Ok(()) if reconnection succeeded, Err otherwise.
    /// On success, resets the poisoned flag and reconnection state.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base.try_reconnect(SocketType::Dealer).await
    }

    /// Check if the socket is poisoned (I/O was cancelled mid-operation).
    #[inline]
    pub fn is_poisoned(&self) -> bool {
        self.base.is_poisoned()
    }

    /// Get the number of currently buffered messages.
    #[inline]
    pub fn buffered_messages(&self) -> usize {
        self.base.buffered_messages()
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }

    /// Receive a message with automatic reconnection on EOF or network error.
    ///
    /// If the socket was created with `connect()` and stores an endpoint, this
    /// method loops: on EOF or broken-pipe it clears the stream and calls
    /// `try_reconnect()` (which applies exponential backoff), then retries `recv()`.
    ///
    /// Respects `max_reconnect_attempts`  -  returns `NotConnected` when exhausted.
    pub async fn recv_with_reconnect(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        let max = self.base.options.max_reconnect_attempts;
        let mut attempts = 0u32;

        loop {
            if self.base.stream.is_none() {
                if let Some(limit) = max {
                    if attempts >= limit {
                        return Err(io::Error::new(
                            io::ErrorKind::NotConnected,
                            format!("Max {} reconnection attempts exceeded", limit),
                        ));
                    }
                }
                attempts += 1;
                trace!(
                    "[DEALER] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.recv().await {
                Ok(Some(msg)) => return Ok(Some(msg)),
                // EOF: read_raw() already set stream = None
                Ok(None) => {
                    debug!("[DEALER] EOF on recv, will reconnect");
                }
                Err(e) => {
                    // If the stream is now None (write_from_buf set it on a
                    // preceding failed send), treat it as a reconnectable error.
                    // Also accept well-known connection-reset codes.
                    if self.base.stream.is_none()
                        || matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::BrokenPipe
                                | io::ErrorKind::UnexpectedEof
                        )
                    {
                        debug!("[DEALER] Connection error on recv ({}), will reconnect", e);
                        self.base.stream = None;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Send a message with automatic reconnection on network error.
    ///
    /// On BrokenPipe / ConnectionReset, `write_from_buf()` already sets
    /// `stream = None`, so the next loop iteration reconnects automatically.
    ///
    /// Respects `max_reconnect_attempts`  -  returns `NotConnected` when exhausted.
    pub async fn send_with_reconnect(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        let max = self.base.options.max_reconnect_attempts;
        let mut attempts = 0u32;

        loop {
            if self.base.stream.is_none() {
                if let Some(limit) = max {
                    if attempts >= limit {
                        return Err(io::Error::new(
                            io::ErrorKind::NotConnected,
                            format!("Max {} reconnection attempts exceeded", limit),
                        ));
                    }
                }
                attempts += 1;
                trace!(
                    "[DEALER] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.send(msg.clone()).await {
                Ok(()) => return Ok(()),
                Err(_) if self.base.stream.is_none() => {
                    // write_from_buf set stream = None → network error, retry
                    debug!("[DEALER] Send failed (stream lost), will reconnect");
                }
                Err(e) => return Err(e),
            }
        }
    }
}

// Implement Socket trait for DealerSocket
crate::impl_socket_trait!(DealerSocket<S>, SocketType::Dealer);

// Specialized implementation for Inproc streams
use crate::inproc_stream::InprocStream;

impl DealerSocket<InprocStream> {
    /// Bind to an inproc endpoint.
    ///
    /// Creates a new inproc endpoint that other sockets can connect to.
    /// Inproc endpoints must be bound before they can be connected to.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Inproc URI (e.g., "inproc://zeromq.zap.01")
    /// * `options` - Socket options
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use monocoque_zmtp::DealerSocket;
    /// use monocoque_zmtp::inproc_stream::InprocStream;
    /// use monocoque_core::options::SocketOptions;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let options = SocketOptions::default();
    /// let mut socket = DealerSocket::<InprocStream>::bind_inproc(
    ///     "inproc://zeromq.zap.01",
    ///     options
    /// )?;
    ///
    /// // ZAP handler logic here
    /// let request = socket.recv().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn bind_inproc(endpoint: &str, options: SocketOptions) -> io::Result<Self> {
        use crate::inproc_stream::InprocStream;
        use monocoque_core::inproc::bind_inproc;

        debug!("[DEALER] Binding to inproc endpoint: {}", endpoint);

        // Bind to inproc endpoint
        let (tx, rx) = bind_inproc(endpoint)?;
        let stream = InprocStream::new(tx, rx);

        debug!("[DEALER] Bound to inproc endpoint: {}", endpoint);

        // Create socket from the stream (inproc doesn't need handshake)
        Ok(Self {
            base: SocketBase::new(stream, SocketType::Dealer, options),
            frames: SmallVec::new(),
        })
    }

    /// Connect to an inproc endpoint.
    ///
    /// Connects to a previously bound inproc endpoint.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Inproc URI (e.g., "inproc://zeromq.zap.01")
    /// * `options` - Socket options
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use monocoque_zmtp::DealerSocket;
    /// use monocoque_zmtp::inproc_stream::InprocStream;
    /// use monocoque_core::options::SocketOptions;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let options = SocketOptions::default();
    /// let mut socket = DealerSocket::<InprocStream>::connect_inproc(
    ///     "inproc://zeromq.zap.01",
    ///     options
    /// )?;
    ///
    /// // Send ZAP request
    /// socket.send(vec![bytes::Bytes::from("request")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn connect_inproc(endpoint: &str, options: SocketOptions) -> io::Result<Self> {
        use crate::inproc_stream::InprocStream;

        debug!("[DEALER] Connecting to inproc endpoint: {}", endpoint);

        // connect_inproc_bidi returns (to_server_tx, from_server_rx) so we can
        // both send to the server and receive replies from it. The server must
        // have been bound with bind_inproc_bidi.
        let (tx, rx) = monocoque_core::inproc::connect_inproc_bidi(endpoint)?;
        let stream = InprocStream::new(tx, rx);

        debug!("[DEALER] Connected to inproc endpoint: {}", endpoint);

        // Create socket from the stream
        Ok(Self {
            base: SocketBase::new(stream, SocketType::Dealer, options),
            frames: SmallVec::new(),
        })
    }
}
