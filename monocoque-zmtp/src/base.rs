//! Base socket infrastructure shared by all ZMQ socket types.
//!
//! This module provides `SocketBase<S>` which contains all common fields and
//! low-level I/O operations used by DEALER, ROUTER, REQ, REP, PUB, SUB sockets.
//!
//! # Design Philosophy
//!
//! - **Zero-cost abstraction**: Composition-based, no vtables or dynamic dispatch
//! - **Single source of truth**: Common logic implemented once
//! - **Type safety**: Generic over stream type `S`
//! - **Protocol safety**: PoisonGuard integration for cancellation safety
//! - **Reconnection support**: Optional endpoint storage and backoff logic

use bytes::BytesMut;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::alloc::{IoArena, IoBytes};
use monocoque_core::buffer::SegmentedBuffer;
use monocoque_core::endpoint::Endpoint;
use monocoque_core::options::SocketOptions;
use monocoque_core::poison::PoisonGuard;
use monocoque_core::reconnect::ReconnectState;
use std::fmt;
use std::io;
use tracing::{debug, trace};

use crate::codec::{ZmtpDecoder, ZmtpFrame};
use crate::handshake::perform_handshake_with_timeout;
use crate::session::SocketType;

/// Base socket infrastructure shared by all ZMQ socket types.
///
/// Contains all common fields and low-level I/O operations. Each socket type
/// (DEALER, ROUTER, REQ, REP, etc.) composes this struct and adds socket-specific
/// logic on top.
///
/// # Fields
///
/// - **Connection state**: `stream`, `endpoint`, `reconnect`
/// - **Buffers**: `recv`, `send_buffer`, `write_buf`, `arena`
/// - **Protocol**: `decoder`, `is_poisoned`
/// - **Configuration**: `config`, `options`
///
/// # Zero-Cost Abstraction
///
/// This is a plain struct with no vtable. The compiler can inline all methods,
/// resulting in zero runtime overhead compared to duplicating code in each socket.
pub struct SocketBase<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Underlying stream (TCP or Unix socket) - None when disconnected
    pub(crate) stream: Option<S>,
    
    /// Optional endpoint for automatic reconnection
    pub(crate) endpoint: Option<Endpoint>,
    
    /// Reconnection state tracker (exponential backoff)
    pub(crate) reconnect: Option<ReconnectState>,
    
    /// ZMTP frame decoder
    pub(crate) decoder: ZmtpDecoder,
    
    /// Arena allocator for zero-copy I/O
    pub(crate) arena: IoArena,
    
    /// Segmented read buffer for incoming data
    pub(crate) recv: SegmentedBuffer,
    
    /// Reusable write buffer for outgoing data
    pub(crate) write_buf: BytesMut,
    
    /// Send buffer for message batching
    pub(crate) send_buffer: BytesMut,
    
    /// Socket options (timeouts, limits, identity, buffer sizes)
    pub(crate) options: SocketOptions,
    
    /// Socket type for introspection (used internally for reconnection)
    #[allow(dead_code)]
    pub(crate) socket_type: SocketType,
    
    /// Last connected/bound endpoint
    pub(crate) last_endpoint: Option<String>,
    
    /// Connection health flag (true if I/O was cancelled mid-operation)
    pub(crate) is_poisoned: bool,
    
    /// Number of messages currently buffered (for HWM enforcement)
    pub(crate) buffered_messages: usize,
}

impl<S> SocketBase<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new SocketBase with the given stream and options.
    ///
    /// This is used when the socket is created from an existing stream
    /// (e.g., `from_tcp`, `from_unix_stream`). No endpoint or reconnection
    /// state is stored.
    ///
    /// Buffer sizes are taken from `options.read_buffer_size` and `options.write_buffer_size`.
    pub fn new(
        stream: S,
        socket_type: SocketType,
        options: SocketOptions,
    ) -> Self {
        let write_capacity = options.write_buffer_size;
        Self {
            stream: Some(stream),
            endpoint: None,
            reconnect: None,
            decoder: ZmtpDecoder::new(),
            arena: IoArena::new(),
            recv: SegmentedBuffer::new(),
            write_buf: BytesMut::with_capacity(write_capacity),
            send_buffer: BytesMut::with_capacity(write_capacity),
            options,
            socket_type,
            last_endpoint: None,
            is_poisoned: false,
            buffered_messages: 0,
        }
    }

    /// Create a new SocketBase with endpoint storage for reconnection.
    ///
    /// This is used when the socket is created via `connect(endpoint)` and
    /// automatic reconnection is desired.
    ///
    /// Buffer sizes are taken from `options.read_buffer_size` and `options.write_buffer_size`.
    pub fn with_endpoint(
        stream: S,
        socket_type: SocketType,
        endpoint: Endpoint,
        options: SocketOptions,
    ) -> Self {
        let endpoint_str = endpoint.to_string();
        let write_capacity = options.write_buffer_size;
        Self {
            stream: Some(stream),
            endpoint: Some(endpoint),
            reconnect: Some(ReconnectState::new(&options)),
            decoder: ZmtpDecoder::new(),
            arena: IoArena::new(),
            recv: SegmentedBuffer::new(),
            write_buf: BytesMut::with_capacity(write_capacity),
            send_buffer: BytesMut::with_capacity(write_capacity),
            options,
            socket_type,
            last_endpoint: Some(endpoint_str),
            is_poisoned: false,
            buffered_messages: 0,
        }
    }

    /// Check if the socket is connected.
    #[inline]
    pub const fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Check if the socket is poisoned (I/O was cancelled mid-operation).
    #[inline]
    pub const fn is_poisoned(&self) -> bool {
        self.is_poisoned
    }

    /// Get the number of buffered messages.
    #[inline]
    pub const fn buffered_messages(&self) -> usize {
        self.buffered_messages
    }

    /// Get the number of buffered bytes.
    #[inline]
    pub fn buffered_bytes(&self) -> usize {
        self.send_buffer.len()
    }

    /// Check if send HWM has been reached.
    #[inline]
    pub const fn hwm_reached(&self) -> bool {
        self.buffered_messages >= self.options.send_hwm
    }

    /// Get the endpoint this socket is connected/bound to, if any.
    ///
    /// Returns `None` if the socket was created from a raw stream without
    /// endpoint information.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub const fn last_endpoint(&self) -> Option<&Endpoint> {
        self.endpoint.as_ref()
    }

    /// Get the last endpoint as a string.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub fn last_endpoint_string(&self) -> Option<&str> {
        self.last_endpoint.as_deref()
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    // socket_type field is used internally for reconnection logic
    // Each socket implementation provides its own public socket_type() method
    /// Check if more message frames are expected (for multipart messages).
    ///
    /// This indicates whether the last received message has more frames
    /// coming after it. Always returns `false` for single-frame messages.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) option.
    pub fn has_more(&self) -> bool {
        self.decoder.has_more()
    }

    /// Get current socket events (read/write readiness).
    ///
    /// Returns a bitmask indicating which operations can proceed without blocking:
    /// - `POLLIN` (1): Socket has messages ready to read
    /// - `POLLOUT` (2): Socket can accept messages for sending
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_EVENTS` (15) option.
    ///
    /// # Note
    ///
    /// This is a best-effort check based on current buffer state.
    /// For true async readiness, use the async recv/send operations.
    #[inline]
    pub fn events(&self) -> u32 {
        let mut events = 0u32;
        
        // POLLIN (1): Can receive if connected and buffers available
        if self.is_connected() && !self.is_poisoned {
            events |= 1; // POLLIN
        }
        
        // POLLOUT (2): Can send if connected and HWM not reached
        if self.is_connected() && !self.hwm_reached() && !self.is_poisoned {
            events |= 2; // POLLOUT
        }
        
        events
    }

    /// Get a mutable reference to the stream, ensuring it's connected.
    ///
    /// Returns `NotConnected` error if the stream is None.
    #[inline]
    // Internal utility method for direct stream access in advanced scenarios
    #[allow(dead_code)]
    pub(crate) fn stream_mut(&mut self) -> io::Result<&mut S> {
        self.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })
    }

    /// Read raw bytes from the stream into the recv buffer without decoding.
    ///
    /// This is the low-level read primitive used by socket implementations to
    /// accumulate multipart messages. Callers should manually decode frames
    /// from the recv buffer using `decoder.decode()`.
    ///
    /// Returns:
    /// - `Ok(n)` where n is the number of bytes read (n > 0)
    /// - `Ok(0)` if EOF was reached (connection closed)
    /// - `Err(e)` on I/O error
    ///
    /// On EOF, sets `stream = None` to mark disconnection.
    pub(crate) async fn read_raw(&mut self) -> io::Result<usize> {
        // Ensure we're connected
        if self.stream.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Socket not connected",
            ));
        }

        // Read from stream
        use compio::buf::BufResult;
        let slab = self.arena.alloc_mut(self.options.read_buffer_size);

        // Get stream reference only for I/O
        let stream = self.stream.as_mut().unwrap(); // Safe: checked above

        // Apply recv timeout
        let BufResult(result, slab) = match self.options.recv_timeout {
            None => AsyncRead::read(stream, slab).await,
            Some(dur) if dur.is_zero() => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Socket is in non-blocking mode and no data is available",
                ));
            }
            Some(dur) => {
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
            trace!("[SocketBase] Connection closed (EOF)");
            self.stream = None;
            return Ok(0);
        }

        // Push bytes into recv buffer
        self.recv.push(slab.freeze());
        Ok(n)
    }

    /// Read a single ZMTP frame from the stream.
    ///
    /// Returns:
    /// - `Ok(Some(frame))` if a complete frame was decoded
    /// - `Ok(None)` if EOF was reached (connection closed)
    /// - `Err(e)` on I/O error
    ///
    /// On EOF, sets `stream = None` to mark disconnection.
    // Low-level frame reader utility - may be used for protocol extensions
    #[allow(dead_code)]
    pub(crate) async fn read_frame(&mut self) -> io::Result<Option<ZmtpFrame>> {
        loop {
            // Try to decode a frame from buffered data
            if let Some(frame) = self.decoder.decode(&mut self.recv)? {
                return Ok(Some(frame));
            }

            // Need more data - read raw bytes
            let n = self.read_raw().await?;
            if n == 0 {
                // EOF
                return Ok(None);
            }
        }
    }

    /// Write buffered data from `send_buffer` to the stream.
    ///
    /// Uses PoisonGuard to ensure cancellation safety. If this method is
    /// cancelled during the write, the socket will be marked poisoned.
    ///
    /// Returns `Ok(())` on success, `Err(e)` on failure. On write failure,
    /// sets `stream = None` to mark disconnection.
    pub(crate) async fn flush_send_buffer(&mut self) -> io::Result<()> {
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

        trace!("[SocketBase] Flushing {} bytes", self.send_buffer.len());

        use compio::buf::BufResult;
        let buf = self.send_buffer.split().freeze();

        // Arm poison guard
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        // Apply send timeout
        let BufResult(result, _) = match self.options.send_timeout {
            None => AsyncWrite::write(stream, IoBytes::new(buf)).await,
            Some(dur) if dur.is_zero() => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Socket is in non-blocking mode and cannot flush immediately",
                ));
            }
            Some(dur) => {
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

        // Success - disarm guard and reset counter
        guard.disarm();
        self.buffered_messages = 0;

        trace!("[SocketBase] Flush completed");
        Ok(())
    }

    /// Write bytes directly to the stream (bypassing send_buffer).
    ///
    /// Uses PoisonGuard for cancellation safety. On write failure,
    /// sets `stream = None` to mark disconnection.
    // Low-level direct write utility - bypasses send_buffer for special cases
    #[allow(dead_code)]
    pub(crate) async fn write_direct(&mut self, data: &[u8]) -> io::Result<()> {
        // Check health
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket poisoned by cancelled I/O",
            ));
        }

        // Ensure we have a connected stream
        let stream = self.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;

        // Arm poison guard
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        // Copy to write buffer and send
        self.write_buf.clear();
        self.write_buf.extend_from_slice(data);
        let buf = self.write_buf.split().freeze();

        use compio::buf::BufResult;
        let BufResult(result, _) = AsyncWrite::write(stream, IoBytes::new(buf)).await;

        // Mark disconnected on error
        if result.is_err() {
            self.stream = None;
        }

        result?;

        guard.disarm();
        Ok(())
    }

    /// Write the contents of write_buf directly to the stream.
    ///
    /// This is used when the caller has already encoded data into write_buf
    /// and wants to send it without additional copying. Applies send_timeout
    /// from options and uses PoisonGuard for cancellation safety.
    pub(crate) async fn write_from_buf(&mut self) -> io::Result<()> {
        // Check health
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket poisoned by cancelled I/O",
            ));
        }

        // Ensure we have a connected stream
        let stream = self.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;

        // Arm poison guard
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        // Send write_buf contents
        let buf = self.write_buf.split().freeze();

        use compio::buf::BufResult;
        
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

        // Mark disconnected on error
        if result.is_err() {
            self.stream = None;
        }

        result?;

        guard.disarm();
        Ok(())
    }
}

impl SocketBase<TcpStream> {
    /// Try to reconnect to the stored endpoint.
    ///
    /// This method:
    /// 1. Checks if endpoint is configured
    /// 2. Applies exponential backoff delay
    /// 3. Attempts new TCP connection
    /// 4. Performs ZMTP handshake
    /// 5. Resets socket state on success
    ///
    /// Returns `Ok(())` on successful reconnection, `Err(e)` otherwise.
    pub(crate) async fn try_reconnect(&mut self, socket_type: SocketType) -> io::Result<()> {
        // Can only reconnect if we have an endpoint
        let endpoint = self.endpoint.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Socket was not created with connect() - no endpoint stored for reconnection",
            )
        })?;

        // Apply backoff delay if we have reconnection state
        if let Some(reconnect) = &mut self.reconnect {
            let delay = reconnect.next_delay();
            debug!(
                "[SocketBase] Reconnection attempt {} after {:?}",
                reconnect.attempt(),
                delay
            );
            compio::time::sleep(delay).await;
        }

        // Attempt connection based on endpoint type
        let mut new_stream = match endpoint {
            Endpoint::Tcp(addr) => TcpStream::connect(addr).await?,
            #[cfg(unix)]
            Endpoint::Ipc(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IPC reconnection not supported for TcpStream base",
                ));
            }
            Endpoint::Inproc(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Inproc reconnection not supported for TcpStream base",
                ));
            }
        };

        // Perform handshake
        perform_handshake_with_timeout(
            &mut new_stream,
            socket_type,
            None, // Identity not supported in reconnection yet
            Some(self.options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed during reconnect: {}", e)))?;

        // Success! Update socket state
        self.stream = Some(new_stream);
        self.is_poisoned = false;
        self.recv = SegmentedBuffer::new();
        self.send_buffer.clear();
        self.buffered_messages = 0;

        // Reset reconnection state
        if let Some(ref mut reconnect) = self.reconnect {
            reconnect.reset();
        }

        debug!("[SocketBase] Reconnection successful");
        Ok(())
    }
}

impl<S> fmt::Debug for SocketBase<S>
where
    S: AsyncRead + AsyncWrite + Unpin + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketBase")
            .field("connected", &self.is_connected())
            .field("poisoned", &self.is_poisoned)
            .field("buffered_messages", &self.buffered_messages)
            .field("buffered_bytes", &self.buffered_bytes())
            .field("endpoint", &self.endpoint)
            .finish()
    }
}
