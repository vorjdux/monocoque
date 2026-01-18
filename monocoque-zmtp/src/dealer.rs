//! Direct-stream DEALER socket implementation
//!
//! This module provides a high-performance DEALER socket using direct stream I/O
//! for minimal latency.
//!
//! # DEALER Pattern
//!
//! DEALER sockets are bidirectional asynchronous sockets that allow sending and
//! receiving messages freely without a strict request-reply pattern.

use bytes::{Bytes, BytesMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::alloc::IoArena;
use monocoque_core::alloc::IoBytes;
use monocoque_core::buffer::SegmentedBuffer;
use monocoque_core::options::SocketOptions;
use monocoque_core::poison::PoisonGuard;
use smallvec::SmallVec;
use std::io;
use std::time::Duration;
use tracing::{debug, trace};

use crate::{
    codec::{encode_multipart, ZmtpDecoder},
    handshake::perform_handshake_with_timeout,
    session::SocketType,
};
use monocoque_core::config::BufferConfig;
use monocoque_core::reconnect::ReconnectState;
use monocoque_core::endpoint::Endpoint;
use std::fmt;

/// Direct-stream DEALER socket with optional auto-reconnection support.
pub struct DealerSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Underlying stream (TCP or Unix socket) - None when disconnected
    stream: Option<S>,
    /// Optional endpoint for automatic reconnection
    endpoint: Option<Endpoint>,
    /// Reconnection state tracker
    reconnect: Option<ReconnectState>,
    /// ZMTP decoder for decoding frames
    decoder: ZmtpDecoder,
    /// Arena for zero-copy allocation
    arena: IoArena,
    /// Segmented read buffer for incoming data
    recv: SegmentedBuffer,
    /// Write buffer for outgoing data (reused to avoid allocations)
    write_buf: BytesMut,
    /// Accumulated frames for current multipart message
    /// SmallVec avoids heap allocation for 1-4 frame messages (common case)
    frames: SmallVec<[Bytes; 4]>,
    /// Buffer configuration
    config: BufferConfig,
    /// Send buffer for batching (explicit flush control)
    send_buffer: BytesMut,
    /// Socket options (timeouts, limits, etc.)
    options: SocketOptions,
    /// Connection health flag (true if I/O was cancelled mid-operation)
    is_poisoned: bool,
    /// Number of messages currently buffered (for HWM enforcement)
    buffered_messages: usize,
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

        // Create arena for zero-copy allocation
        let arena = IoArena::new();

        // Create ZMTP decoder
        let decoder = ZmtpDecoder::new();

        // Create buffers
        let recv = SegmentedBuffer::new();
        let write_buf = BytesMut::with_capacity(config.write_buf_size);

        debug!("[DEALER] Socket initialized");

        Ok(Self {
            stream: Some(stream),
            endpoint: None,
            reconnect: None,
            decoder,
            arena,
            recv,
            write_buf,
            frames: SmallVec::new(),
            config,
            send_buffer: BytesMut::new(),
            options,
            is_poisoned: false,
            buffered_messages: 0,
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

        // Create socket with reconnection enabled
        let arena = IoArena::new();
        let decoder = ZmtpDecoder::new();
        let recv = SegmentedBuffer::new();
        let write_buf = BytesMut::with_capacity(config.write_buf_size);
        let reconnect_state = ReconnectState::new(&options);

        Ok(Self {
            stream: Some(stream),
            endpoint: Some(parsed_endpoint),
            reconnect: Some(reconnect_state),
            decoder,
            arena,
            recv,
            write_buf,
            frames: SmallVec::new(),
            config,
            send_buffer: BytesMut::new(),
            options,
            is_poisoned: false,
            buffered_messages: 0,
        })
    }

    /// Try to reconnect to the stored endpoint.
    ///
    /// Returns Ok(()) if reconnection succeeded, Err otherwise.
    /// On success, resets the poisoned flag and reconnection state.
    async fn try_reconnect(&mut self) -> io::Result<()>
    where
        S: From<TcpStream> + fmt::Debug,
    {
        use compio::net::TcpStream;

        // Can only reconnect if we have an endpoint
        let endpoint = self.endpoint.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "No endpoint configured for reconnection - use connect() API"
            )
        })?;

        debug!("[DEALER] Attempting reconnection to {}", endpoint);

        // Connect based on endpoint type
        let mut new_stream = match endpoint {
            Endpoint::Tcp(addr) => {
                let tcp_stream = TcpStream::connect(addr).await?;
                S::from(tcp_stream)
            }
            #[cfg(unix)]
            Endpoint::Ipc(_path) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IPC reconnection not yet supported"
                ));
            }
        };

        // Perform handshake
        perform_handshake_with_timeout(
            &mut new_stream,
            SocketType::Dealer,
            None,
            Some(self.options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Reconnection handshake failed: {}", e)))?;

        // Success! Replace stream and reset state
        self.stream = Some(new_stream);
        self.is_poisoned = false;
        self.buffered_messages = 0;
        self.send_buffer.clear();

        if let Some(ref mut reconnect) = self.reconnect {
            reconnect.reset();
        }

        debug!("[DEALER] Reconnection successful");
        Ok(())
    }

    /// Receive a message.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[DEALER] Waiting for message");

        // Ensure we have a connected stream
        let stream = self.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;

        // Read from stream until we have a complete message
        loop {
            // Try to decode frames from buffer
            loop {
                match self.decoder.decode(&mut self.recv)? {
                    Some(frame) => {
                        let more = frame.more();
                        self.frames.push(frame.payload);

                        if !more {
                            // Complete message received
                            // Collect frames while preserving capacity
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[DEALER] Received {} frames", msg.len());
                            return Ok(Some(msg));
                        }
                    }
                    None => break, // Need more data
                }
            }

            // Need more data - read from stream using reused buffer
            use compio::buf::BufResult;

            let slab = self.arena.alloc_mut(self.config.read_buf_size);
            
            // Apply recv timeout from options
            let BufResult(result, slab) = match self.options.recv_timeout {
                None => {
                    // Blocking mode - no timeout
                    AsyncRead::read(stream, slab).await
                }
                Some(dur) if dur.is_zero() => {
                    // Non-blocking mode - return WouldBlock immediately if not ready
                    // compio doesn't directly support non-blocking, so we use a minimal timeout
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Socket is in non-blocking mode and no data is available",
                    ));
                }
                Some(dur) => {
                    // Timed mode - apply timeout
                    use compio::time::timeout;
                    match timeout(dur, AsyncRead::read(stream, slab)).await {
                        Ok(result) => result,
                        Err(_) => {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                format!("Receive operation timed out after {:?}", dur),
                            ));
                        }
                    }
                }
            };
            
            let n = result?;

            if n == 0 {
                // EOF - mark stream as disconnected
                trace!("[DEALER] Connection closed");
                self.stream = None;
                return Ok(None);
            }

            // Push bytes into segmented recv queue (zero-copy)
            self.recv.push(slab.freeze());
        }
    }

    /// Send a message immediately.
    ///
    /// Encodes and sends the message in a single I/O operation.
    /// For high-throughput scenarios, consider using `send_buffered()` + `flush()`
    /// to batch multiple messages.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        // Check health before attempting I/O
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket poisoned by cancelled I/O - reconnect required",
            ));
        }

        // Ensure we have a connected stream
        let stream = self.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;

        trace!("[DEALER] Sending {} frames", msg.len());

        // Encode message - reuse write_buf to avoid allocation
        self.write_buf.clear();
        encode_multipart(&msg, &mut self.write_buf);

        // Write to stream using compio
        use compio::buf::BufResult;

        let buf = self.write_buf.split().freeze();
        
        // Arm the guard - if dropped before disarm, socket remains poisoned
        let guard = PoisonGuard::new(&mut self.is_poisoned);
        
        // Apply send timeout from options
        let BufResult(result, _) = match self.options.send_timeout {
            None => {
                // Blocking mode - no timeout
                AsyncWrite::write(stream, IoBytes::new(buf)).await
            }
            Some(dur) if dur.is_zero() => {
                // Non-blocking mode
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Socket is in non-blocking mode and cannot send immediately",
                ));
            }
            Some(dur) => {
                // Timed mode - apply timeout
                use compio::time::timeout;
                match timeout(dur, AsyncWrite::write(stream, IoBytes::new(buf))).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("Send operation timed out after {:?}", dur),
                        ));
                    }
                }
            }
        };
        
        let write_result = result;

        // If write failed, mark stream as disconnected
        if write_result.is_err() {
            self.stream = None;
        }

        write_result?;

        // Success - disarm the guard
        guard.disarm();

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
        if self.buffered_messages >= self.options.send_hwm {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "Send high water mark reached ({} messages). Flush or drop messages.",
                    self.options.send_hwm
                ),
            ));
        }

        trace!("[DEALER] Buffering {} frames", msg.len());

        // Encode directly into send_buffer
        encode_multipart(&msg, &mut self.send_buffer);
        self.buffered_messages += 1;
        Ok(())
    }

    /// Flush all buffered messages to the network.
    ///
    /// Sends all messages buffered by `send_buffered()` in a single I/O operation.
    pub async fn flush(&mut self) -> io::Result<()> {
        if self.send_buffer.is_empty() {
            return Ok(());
        }

        // Check health before attempting I/O
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket poisoned by cancelled I/O - reconnect required",
            ));
        }

        // Ensure we have a connected stream
        let stream = self.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;

        trace!("[DEALER] Flushing {} bytes", self.send_buffer.len());

        // Write buffered data
        use compio::buf::BufResult;
        let buf = self.send_buffer.split().freeze();
        
        // Arm the guard - if dropped before disarm, socket remains poisoned
        let guard = PoisonGuard::new(&mut self.is_poisoned);
        
        // Apply send timeout from options
        let BufResult(result, _) = match self.options.send_timeout {
            None => {
                // Blocking mode - no timeout
                AsyncWrite::write(stream, IoBytes::new(buf)).await
            }
            Some(dur) if dur.is_zero() => {
                // Non-blocking mode
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Socket is in non-blocking mode and cannot flush immediately",
                ));
            }
            Some(dur) => {
                // Timed mode - apply timeout
                use compio::time::timeout;
                match timeout(dur, AsyncWrite::write(stream, IoBytes::new(buf))).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("Flush operation timed out after {:?}", dur),
                        ));
                    }
                }
            }
        };
        
        let write_result = result;

        // If write failed, mark stream as disconnected
        if write_result.is_err() {
            self.stream = None;
        }

        write_result?;

        // Success - disarm the guard and reset counter
        guard.disarm();
        self.buffered_messages = 0;

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
            if self.buffered_messages >= self.options.send_hwm {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "Send high water mark reached ({} messages)",
                        self.options.send_hwm
                    ),
                ));
            }
            encode_multipart(msg, &mut self.send_buffer);
            self.buffered_messages += 1;
        }

        self.flush().await
    }

    /// Get the number of bytes currently buffered.
    #[inline]
    pub fn buffered_bytes(&self) -> usize {
        self.send_buffer.len()
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
        let linger = self.options.linger;
        
        // If no data buffered, just drop the socket
        if self.send_buffer.is_empty() {
            trace!("[DEALER] No buffered data, closing immediately");
            return Ok(());
        }

        trace!(
            "[DEALER] Closing with {} bytes buffered, linger={:?}",
            self.send_buffer.len(),
            linger
        );

        match linger {
            Some(dur) if dur.is_zero() => {
                // Linger = 0: discard buffered data immediately
                debug!("[DEALER] Linger=0, discarding {} bytes", self.send_buffer.len());
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
        &self.options
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
        &mut self.options
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
        self.options = options;
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

    /// Receive a message with automatic reconnection.
    ///
    /// If the socket was created with `connect()` and has an endpoint configured,
    /// this will automatically attempt to reconnect on disconnection.
    ///
    /// For sockets created with `from_tcp()`, this behaves the same as `recv()`.
    pub async fn recv_with_reconnect(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // Try to reconnect if disconnected
        if self.stream.is_none() {
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
        if self.stream.is_none() {
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
