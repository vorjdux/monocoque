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
use monocoque_core::config::BufferConfig;
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
}

impl<S> ReqSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new REQ socket from a stream.
    ///
    /// This performs the ZMTP handshake and initializes the socket.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Handshake fails
    /// - Connection is closed during handshake
    ///
    /// # Example
    ///
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
        // REQ sockets typically handle low-latency RPC with small messages
        Self::with_options(stream, BufferConfig::small(), SocketOptions::default()).await
    }

    /// Create a new REQ socket from a stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency request/reply with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput with large messages
    pub async fn with_config(stream: S, config: BufferConfig) -> io::Result<Self> {
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new REQ socket with custom buffer configuration and socket options.
    pub async fn with_options(
        mut stream: S,
        config: BufferConfig,
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
            base: SocketBase::new(stream, config, options),
            frames: SmallVec::new(),
            state: ReqState::Idle,
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
        // Check state machine
        if self.state != ReqState::Idle {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot send while awaiting reply - must call recv() first",
            ));
        }

        trace!("[REQ] Sending {} frames", msg.len());

        // Encode message into write_buf
        self.base.write_buf.clear();
        encode_multipart(&msg, &mut self.base.write_buf);

        // Delegate to base for writing
        self.base.write_from_buf().await?;

        // Transition to awaiting reply
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
                            self.state = ReqState::Idle;
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
    /// Create a new REQ socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_config(stream, BufferConfig::large()).await
    }

    /// Create a new REQ socket from a TCP stream with TCP_NODELAY and custom config.
    pub async fn from_tcp_with_config(
        stream: TcpStream,
        config: BufferConfig,
    ) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[REQ] TCP_NODELAY enabled");
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new REQ socket from a TCP stream with TCP_NODELAY and custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[REQ] TCP_NODELAY enabled");
        Self::with_options(stream, config, options).await
    }
}
