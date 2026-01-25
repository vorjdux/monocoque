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
//! compio::net::TcpStream (io_uring)
//! ```
//!
//! # Example
//!
//! ```rust,no_run
//! use monocoque_zmtp::req::ReqSocket;
//! use compio::net::TcpStream;
//! use bytes::Bytes;
//!
//! #[compio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let stream = TcpStream::connect("127.0.0.1:5555").await?;
//!     let mut socket = ReqSocket::new(stream).await?;
//!     
//!     // Send request
//!     socket.send(vec![Bytes::from("Hello")]).await?;
//!     
//!     // Receive reply
//!     let reply = socket.recv().await?;
//!     
//!     Ok(())
//! }
//! ```

use crate::base::SocketBase;
use crate::codec::encode_multipart;
use crate::{
    handshake::perform_handshake_with_timeout,
    session::SocketType,
};
use monocoque_core::endpoint::Endpoint;
use monocoque_core::options::SocketOptions;
use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
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
/// zero-copy arena allocation for maximum performance.
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
/// use compio::net::TcpStream;
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
    /// use compio::net::TcpStream;
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
    /// # use compio::net::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let mut opts = SocketOptions::small(); // 4KB for low latency
    /// opts.req_correlate = true; // Enable request ID correlation
    /// let socket = ReqSocket::with_options(stream, opts).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_options(
        mut stream: S,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[REQ] Creating new direct REQ socket");

        // Perform ZMTP handshake
        debug!("[REQ] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Req,
            None,
            Some(options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[REQ] Handshake complete"
        );

        debug!("[REQ] Socket initialized");

        Ok(Self {
            base: SocketBase::new(stream, SocketType::Req, options),
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
            
            trace!("[REQ] Correlation enabled, prepending request ID: {}", self.request_id);
            
            // Prepend request ID as first frame (4 bytes, big-endian)
            let mut correlated_msg = Vec::with_capacity(msg.len() + 1);
            correlated_msg.push(Bytes::copy_from_slice(&self.request_id.to_be_bytes()));
            correlated_msg.extend(msg);
            correlated_msg
        } else {
            msg
        };

        // Encode message into write_buf
        self.base.write_buf.clear();
        encode_multipart(&frames_to_send, &mut self.base.write_buf);

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
                match self.base.decoder.decode(&mut self.base.recv)? {
                    Some(frame) => {
                        let more = frame.more();
                        self.frames.push(frame.payload);

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
                                        format!("Correlation frame has invalid length: {} (expected 4)", id_frame.len()),
                                    ));
                                }
                                
                                let received_id = u32::from_be_bytes([
                                    id_frame[0], id_frame[1], id_frame[2], id_frame[3]
                                ]);
                                
                                trace!("[REQ] Received correlation ID: {}", received_id);
                                
                                // Validate against expected ID
                                if let Some(expected) = self.expected_request_id {
                                    if received_id != expected {
                                        return Err(io::Error::new(
                                            io::ErrorKind::InvalidData,
                                            format!("Request ID mismatch: expected {}, got {}", expected, received_id),
                                        ));
                                    }
                                    trace!("[REQ] Correlation ID validated successfully");
                                }
                                
                                // Strip correlation frame and return rest
                                msg[1..].to_vec()
                            } else {
                                msg
                            };
                            
                            self.state = ReqState::Idle;
                            self.expected_request_id = None;
                            return Ok(Some(validated_msg));
                        }
                    }
                    None => break, // Need more data
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
    pub fn stream_ref(&self) -> Option<&S> {
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
    pub fn options(&self) -> &SocketOptions {
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
        self.base.options = options;
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub fn socket_type(&self) -> SocketType {
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
    /// # use compio::net::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect(\"127.0.0.1:5555\").await?;
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
}

crate::impl_socket_trait!(ReqSocket<S>, SocketType::Req);
