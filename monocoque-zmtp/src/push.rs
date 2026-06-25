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
use crate::{handshake::perform_handshake_with_options, session::SocketType};
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
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[PUSH] Creating new PUSH socket");

        // Perform ZMTP handshake
        debug!("[PUSH] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Push,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PUSH] Handshake complete"
        );

        debug!("[PUSH] Socket initialized");

        let mut base = SocketBase::new(stream, SocketType::Push, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self { base })
    }

    /// Send a message to a connected PULL socket.
    ///
    /// Messages are distributed in a round-robin fashion when multiple
    /// PULL sockets are connected (in a multi-connection scenario).
    ///
    /// By default each call writes to the kernel immediately (eager mode, one
    /// io_uring operation per message). For throughput-bound pipelines, enable write
    /// coalescing via [`SocketOptions::with_write_coalescing`] and call
    /// [`flush`](Self::flush) after the last send in each burst. In coalesced mode,
    /// bytes may remain in userspace until the 64 KB threshold fills or `flush()` is
    /// called explicitly.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket is poisoned, disconnected, or if the write fails.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[PUSH] Sending {} frames", msg.len());

        if self.base.options.write_coalescing {
            self.base.send_coalesced(&msg).await?;
        } else {
            self.base.encode_message_to_write_buf(&msg)?;
            self.base.write_from_buf().await?;
        }

        // Check heartbeat: send PING if the connection has been idle too long
        if self.base.check_heartbeat()? {
            self.base.flush_send_buffer().await?;
        }

        trace!("[PUSH] Message sent successfully");
        Ok(())
    }

    /// Flush any messages still buffered by write coalescing.
    ///
    /// Call this after the last `send()` in a burst when `write_coalescing` is
    /// enabled to ensure all pending data is written to the kernel.
    pub async fn flush(&mut self) -> io::Result<()> {
        self.base.flush_send_buffer().await
    }

    /// Close the socket gracefully by shutting down the underlying stream.
    pub async fn close(mut self) -> io::Result<()> {
        trace!("[PUSH] Closing socket");
        self.base.close().await
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

    /// Connect to a remote PUSH socket, storing the endpoint for automatic reconnection.
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
        crate::utils::configure_tcp_stream(&stream, &options, "PUSH")?;

        let mut stream = stream;
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Push,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PUSH] Connected to {} (endpoint stored for reconnection)",
            peer_addr
        );

        let endpoint = monocoque_core::endpoint::Endpoint::Tcp(peer_addr);
        let mut base =
            crate::base::SocketBase::with_endpoint(stream, SocketType::Push, endpoint, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self { base })
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }

    /// Try to reconnect to the stored endpoint.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base.try_reconnect(SocketType::Push).await
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
                    "[PUSH] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.send(msg.clone()).await {
                Ok(()) => return Ok(()),
                Err(_) if self.base.stream.is_none() => {
                    // write_from_buf set stream = None → network error, retry
                    debug!("[PUSH] Send failed (stream lost), will reconnect");
                }
                Err(e) => return Err(e),
            }
        }
    }
}

crate::impl_socket_trait!(PushSocket<S>, SocketType::Push);
