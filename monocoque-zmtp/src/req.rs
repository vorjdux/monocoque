//! Direct-stream REQ socket implementation.
//!
//! This module provides a high-performance REQ socket using direct stream I/O.
//!
//! # Performance Characteristics
//!
//! - **No channel overhead**: Direct stream access
//! - **Zero-copy**: Arena-backed allocation with io_uring owned buffers
//! - **Efficient I/O**: compio's io_uring for minimal syscall overhead
//!
//! # Architecture
//!
//! ```text
//! Application
//!     ↕
//! ReqSocket (state machine)
//!     ↕
//! ZmtpDecoder + SegmentedBuffer
//!     ↕
//! monocoque_core::rt::TcpStream (io_uring)
//! ```
//!
//! # Example
//!
//! ```rust,no_run
//! use monocoque_zmtp::req::ReqSocket;
//! use monocoque_core::rt::TcpStream;
//! use bytes::Bytes;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let stream = TcpStream::connect("127.0.0.1:5555").await?;
//! let mut socket = ReqSocket::new(stream).await?;
//!
//! // Send request
//! socket.send(vec![Bytes::from("Hello")]).await?;
//!
//! // Receive reply
//! let reply = socket.recv().await?;
//!
//! # Ok(())
//! # }
//! ```

use crate::base::SocketBase;
use crate::{handshake::perform_handshake_with_options, session::SocketType};
use bytes::Bytes;
use compio_io::{AsyncRead, AsyncWrite};
use monocoque_core::endpoint::Endpoint;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::TcpStream;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

/// REQ socket state for enforcing strict request-reply pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReqState {
    /// Ready to send a request
    Idle,
    /// Waiting for a reply after sending request
    AwaitingReply,
}

/// High-performance REQ socket using direct stream I/O.
///
/// This implementation uses compio's native owned-buffer API with
/// zero-copy owned buffers for maximum performance.
///
/// # State Machine
///
/// ```text
/// Idle → send() → AwaitingReply → recv() → Idle
/// ```
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::req::ReqSocket;
/// use monocoque_core::rt::TcpStream;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = TcpStream::connect("127.0.0.1:5555").await?;
/// let mut socket = ReqSocket::new(stream).await?;
///
/// // Request-reply cycle
/// socket.send(vec![Bytes::from("REQUEST")]).await?;
/// let reply = socket.recv().await?;
/// # Ok(())
/// # }
/// ```
pub struct ReqSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    /// SmallVec avoids heap allocation for 1-4 frame messages (common case)
    frames: SmallVec<[Bytes; 4]>,
    /// Current state of the REQ state machine
    state: ReqState,
    /// Request ID counter for correlation tracking
    request_id: u32,
    /// Expected request ID for correlation mode (when req_correlate is enabled)
    expected_request_id: Option<u32>,
}

impl<S> ReqSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new REQ socket from a stream using default options.
    ///
    /// This performs the ZMTP handshake and initializes the socket.
    /// Uses default buffer sizes (8KB) for balanced performance.
    ///
    /// # Example
    /// ```rust,no_run
    /// use monocoque_zmtp::req::ReqSocket;
    /// use monocoque_core::rt::TcpStream;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = ReqSocket::new(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new REQ socket with custom socket options.
    ///
    /// # Buffer Configuration
    /// - Use `SocketOptions::small()` (4KB) for low-latency request/reply with small messages
    /// - Use `SocketOptions::large()` (16KB) for high-throughput with large messages
    /// - Use `SocketOptions::default()` (8KB) for balanced workloads
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::req::ReqSocket;
    /// # use monocoque_core::options::SocketOptions;
    /// # use monocoque_core::rt::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let mut opts = SocketOptions::small(); // 4KB for low latency
    /// opts.req_correlate = true; // Enable request ID correlation
    /// let socket = ReqSocket::with_options(stream, opts).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[REQ] Creating new direct REQ socket");

        // Perform ZMTP handshake
        debug!("[REQ] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Req,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[REQ] Handshake complete"
        );

        debug!("[REQ] Socket initialized");

        let mut base = SocketBase::new(stream, SocketType::Req, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
            state: ReqState::Idle,
            request_id: 0,
            expected_request_id: None,
        })
    }

    /// Send a request message.
    ///
    /// This enforces the REQ state machine - you must call `recv()` before
    /// calling `send()` again.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called while awaiting a reply (must call `recv()` first)
    /// - I/O error occurs during send
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::req::ReqSocket;
    /// # use bytes::Bytes;
    /// # async fn example(socket: &mut ReqSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// socket.send(vec![Bytes::from("REQUEST")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        // Check state machine (unless in relaxed mode)
        if !self.base.options.req_relaxed && self.state != ReqState::Idle {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot send while awaiting reply - must call recv() first (use req_relaxed mode to allow multiple outstanding requests)",
            ));
        }

        trace!("[REQ] Sending {} frames", msg.len());

        // If correlation is enabled, prepend request ID as an envelope
        let frames_to_send = if self.base.options.req_correlate {
            // Increment request ID
            self.request_id = self.request_id.wrapping_add(1);
            self.expected_request_id = Some(self.request_id);

            trace!(
                "[REQ] Correlation enabled, prepending request ID: {}",
                self.request_id
            );

            // Prepend request ID as first frame (4 bytes, big-endian)
            let mut correlated_msg = Vec::with_capacity(msg.len() + 1);
            correlated_msg.push(Bytes::copy_from_slice(&self.request_id.to_be_bytes()));
            correlated_msg.extend(msg);
            correlated_msg
        } else {
            msg
        };

        // Encode message into write_buf (with CURVE encryption if active)
        self.base.encode_message_to_write_buf(&frames_to_send)?;

        // Delegate to base for writing
        self.base.write_from_buf().await?;

        // Transition to awaiting reply (unless already there in relaxed mode)
        self.state = ReqState::AwaitingReply;

        trace!("[REQ] Message sent successfully");
        Ok(())
    }

    /// Receive a reply message.
    ///
    /// This blocks until a reply is received. You must call this after `send()`
    /// before calling `send()` again.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(msg))` - Received a multipart message
    /// - `Ok(None)` - Connection closed gracefully
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called while in Idle state (must call `send()` first)
    /// - I/O error occurs during receive
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::req::ReqSocket;
    /// # async fn example(socket: &mut ReqSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// let reply = socket.recv().await?;
    /// if let Some(msg) = reply {
    ///     println!("Got {} frames", msg.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // Check state machine
        if self.state != ReqState::AwaitingReply {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot recv while in Idle state - must call send() first",
            ));
        }

        trace!("[REQ] Waiting for reply");

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
                            // Complete message received
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[REQ] Received {} frames", msg.len());

                            // If correlation is enabled, validate request ID
                            let validated_msg = if self.base.options.req_correlate {
                                if msg.is_empty() {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Correlation enabled but received empty message",
                                    ));
                                }

                                // First frame should be the request ID (4 bytes)
                                let id_frame = &msg[0];
                                if id_frame.len() != 4 {
                                    return Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        format!(
                                            "Correlation frame has invalid length: {} (expected 4)",
                                            id_frame.len()
                                        ),
                                    ));
                                }

                                let received_id = u32::from_be_bytes([
                                    id_frame[0],
                                    id_frame[1],
                                    id_frame[2],
                                    id_frame[3],
                                ]);

                                trace!("[REQ] Received correlation ID: {}", received_id);

                                // Validate against expected ID
                                if let Some(expected) = self.expected_request_id {
                                    if received_id != expected {
                                        return Err(io::Error::new(
                                            io::ErrorKind::InvalidData,
                                            format!(
                                                "Request ID mismatch: expected {}, got {}",
                                                expected, received_id
                                            ),
                                        ));
                                    }
                                    trace!("[REQ] Correlation ID validated successfully");
                                }

                                // Strip correlation frame and return the remaining owned frames.
                                let mut msg = msg;
                                msg.remove(0);
                                msg
                            } else {
                                msg
                            };

                            self.state = ReqState::Idle;
                            self.expected_request_id = None;
                            return Ok(Some(validated_msg));
                        }
                    }
                }
            }

            // Need more data - read raw bytes from stream
            let n = self.base.read_raw().await?;
            if n == 0 {
                // EOF - connection closed
                trace!("[REQ] Connection closed");
                self.state = ReqState::Idle;
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
            // Continue decoding with new data
        }
    }

    /// Get the current state of the REQ socket.
    ///
    /// This is primarily for debugging and testing.
    pub const fn state(&self) -> ReqState {
        self.state
    }

    /// Get a reference to the underlying stream.
    pub const fn stream_ref(&self) -> Option<&S> {
        self.base.stream.as_ref()
    }

    /// Get a mutable reference to the underlying stream.
    pub fn stream_mut(&mut self) -> Option<&mut S> {
        self.base.stream.as_mut()
    }

    /// Close the socket gracefully.
    ///
    /// REQ sockets send immediately (no buffering), so this simply drops the socket.
    /// The linger option is not applicable to REQ sockets.
    pub async fn close(self) -> io::Result<()> {
        trace!("[REQ] Closing socket");
        Ok(())
    }

    /// Get a reference to the socket options.
    #[inline]
    pub const fn options(&self) -> &SocketOptions {
        &self.base.options
    }

    /// Get a mutable reference to the socket options.
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.base.options
    }

    /// Set socket options (builder-style).
    #[inline]
    pub fn set_options(&mut self, options: SocketOptions) {
        self.base.set_options(options);
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type(&self) -> SocketType {
        SocketType::Req
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_req_state_transitions() {
        // Test state equality
        assert_eq!(ReqState::Idle, ReqState::Idle);
        assert_eq!(ReqState::AwaitingReply, ReqState::AwaitingReply);
        assert_ne!(ReqState::Idle, ReqState::AwaitingReply);
    }

    #[test]
    fn test_compio_stream_creation() {
        // Validate CompioStream can be created
        // Actual I/O tests require a real connection
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl ReqSocket<TcpStream> {
    /// Create a REQ socket from a TCP stream with default options.
    ///
    /// Automatically enables TCP_NODELAY and applies TCP keepalive settings.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a REQ socket from a TCP stream with custom socket options.
    ///
    /// Automatically enables TCP_NODELAY and applies TCP keepalive settings from options.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::req::ReqSocket;
    /// # use monocoque_core::options::SocketOptions;
    /// # use monocoque_core::rt::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let mut opts = SocketOptions::small();
    /// opts.req_correlate = true; // Enable request ID correlation
    /// let socket = ReqSocket::from_tcp_with_options(stream, opts).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Apply TCP-specific configuration
        crate::utils::configure_tcp_stream(&stream, &options, "REQ")?;

        Self::with_options(stream, options).await
    }

    /// Connect to a remote REQ socket, storing the endpoint for automatic reconnection.
    pub async fn connect(addr: impl monocoque_core::rt::ToSocketAddrs) -> io::Result<Self> {
        Self::connect_with_options(addr, SocketOptions::default()).await
    }

    /// Connect with custom options, storing the endpoint for reconnection.
    pub async fn connect_with_options(
        addr: impl monocoque_core::rt::ToSocketAddrs,
        options: SocketOptions,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        let peer_addr = stream.peer_addr()?;
        crate::utils::configure_tcp_stream(&stream, &options, "REQ")?;

        let mut stream = stream;
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Req,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[REQ] Connected to {} (endpoint stored for reconnection)",
            peer_addr
        );

        let endpoint = monocoque_core::endpoint::Endpoint::Tcp(peer_addr);
        let mut base =
            crate::base::SocketBase::with_endpoint(stream, SocketType::Req, endpoint, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
            state: ReqState::Idle,
            request_id: 0,
            expected_request_id: None,
        })
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }

    /// Try to reconnect to the stored endpoint.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base.try_reconnect(SocketType::Req).await
    }

    /// Receive a reply with automatic reconnection on EOF or network error.
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
                if let Some(limit) = max
                    && attempts >= limit
                {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        format!("Max {} reconnection attempts exceeded", limit),
                    ));
                }
                attempts += 1;
                trace!(
                    "[REQ] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.state = ReqState::Idle;
                self.expected_request_id = None;
                self.try_reconnect().await?;
            }

            match self.recv().await {
                Ok(Some(msg)) => return Ok(Some(msg)),
                // EOF: read_raw() already set stream = None
                Ok(None) => {
                    debug!("[REQ] EOF on recv, will reconnect");
                    self.state = ReqState::Idle;
                    self.expected_request_id = None;
                }
                Err(e) => {
                    if self.base.stream.is_none()
                        || matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::BrokenPipe
                                | io::ErrorKind::UnexpectedEof
                        )
                    {
                        debug!("[REQ] Connection error on recv ({}), will reconnect", e);
                        self.base.stream = None;
                        self.state = ReqState::Idle;
                        self.expected_request_id = None;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Send a request with automatic reconnection on network error.
    ///
    /// On BrokenPipe / ConnectionReset, `write_from_buf()` already sets
    /// `stream = None`, so the next loop iteration reconnects automatically.
    /// On reconnect the REQ state machine is reset to `Idle`.
    ///
    /// Respects `max_reconnect_attempts`  -  returns `NotConnected` when exhausted.
    pub async fn send_with_reconnect(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        let max = self.base.options.max_reconnect_attempts;
        let mut attempts = 0u32;

        loop {
            if self.base.stream.is_none() {
                if let Some(limit) = max
                    && attempts >= limit
                {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        format!("Max {} reconnection attempts exceeded", limit),
                    ));
                }
                attempts += 1;
                trace!(
                    "[REQ] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.state = ReqState::Idle;
                self.expected_request_id = None;
                self.try_reconnect().await?;
            }

            match self.send(msg.clone()).await {
                Ok(()) => return Ok(()),
                Err(_) if self.base.stream.is_none() => {
                    // write_from_buf set stream = None → network error, retry
                    debug!("[REQ] Send failed (stream lost), will reconnect");
                    self.state = ReqState::Idle;
                    self.expected_request_id = None;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

crate::impl_socket_trait!(ReqSocket<S>, SocketType::Req);
