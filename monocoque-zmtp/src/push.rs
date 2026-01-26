//! PUSH socket implementation
//!
//! PUSH sockets are send-only endpoints in the pipeline pattern. They distribute
//! messages in a round-robin fashion to connected PULL sockets.
//!
//! # Characteristics
//!
//! - **Send-only**: Cannot receive messages
//! - **Load balancing**: Distributes work across PULL sockets
//! - **Non-blocking**: Never blocks on slow receivers (drops if HWM reached)
//! - **Pipeline pattern**: For distributing tasks to workers
//!
//! # Use Cases
//!
//! - Task distribution (ventilator pattern)
//! - Parallel pipeline processing
//! - Work queue distribution

use crate::base::SocketBase;
use crate::codec::encode_multipart;
use crate::{handshake::perform_handshake_with_timeout, session::SocketType};
use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::options::SocketOptions;
use std::io;
use tracing::{debug, trace};

/// PUSH socket for distributing messages in a pipeline.
///
/// PUSH sockets send messages to connected PULL sockets in a round-robin
/// fashion, providing load balancing for parallel processing.
pub struct PushSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
}

impl<S> PushSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new PUSH socket from a stream with default buffer configuration.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PUSH socket with custom buffer configuration and socket options.
    pub async fn with_options(
        mut stream: S,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[PUSH] Creating new PUSH socket");

        // Perform ZMTP handshake
        debug!("[PUSH] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Push,
            None,
            Some(options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PUSH] Handshake complete"
        );

        debug!("[PUSH] Socket initialized");

        Ok(Self {
            base: SocketBase::new(stream, SocketType::Push, options),
        })
    }

    /// Send a message to a connected PULL socket.
    ///
    /// Messages are distributed in a round-robin fashion when multiple
    /// PULL sockets are connected (in a multi-connection scenario).
    ///
    /// # Errors
    ///
    /// Returns an error if the socket is poisoned, disconnected, or if the write fails.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[PUSH] Sending {} frames", msg.len());

        // Encode message into write_buf
        self.base.write_buf.clear();
        encode_multipart(&msg, &mut self.base.write_buf);

        // Delegate to base for writing
        self.base.write_from_buf().await?;

        trace!("[PUSH] Message sent successfully");
        Ok(())
    }

    /// Close the socket gracefully.
    pub async fn close(self) -> io::Result<()> {
        trace!("[PUSH] Closing socket");
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
        self.base.options = options;
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl PushSocket<TcpStream> {
    /// Create a new PUSH socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PUSH socket from a TCP stream with TCP_NODELAY and custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Configure TCP optimizations including keepalive
        crate::utils::configure_tcp_stream(&stream, &options, "PUSH")?;
        Self::with_options(stream, options).await
    }
}

crate::impl_socket_trait!(PushSocket<S>, SocketType::Push);
