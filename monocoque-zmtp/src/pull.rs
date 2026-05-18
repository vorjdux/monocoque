//! PULL socket implementation
//!
//! PULL sockets are receive-only endpoints in the pipeline pattern. They receive
//! messages from connected PUSH sockets in a fair-queued manner.
//!
//! # Characteristics
//!
//! - **Receive-only**: Cannot send messages
//! - **Fair-queued**: Receives from all PUSH sockets fairly
//! - **Pipeline pattern**: For receiving tasks from distributors
//! - **No filtering**: All messages are delivered
//!
//! # Use Cases
//!
//! - Task receiver (worker pattern)
//! - Parallel pipeline processing
//! - Work queue consumption

use crate::base::SocketBase;
use crate::{handshake::perform_handshake_with_options, session::SocketType};
use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::options::SocketOptions;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

/// PULL socket for receiving messages in a pipeline.
///
/// PULL sockets receive messages from connected PUSH sockets, providing
/// the worker side of the pipeline pattern.
pub struct PullSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
}

impl<S> PullSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new PULL socket from a stream with default buffer configuration.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PULL socket with custom buffer configuration and socket options.
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[PULL] Creating new PULL socket");

        // Perform ZMTP handshake
        debug!("[PULL] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Pull,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PULL] Handshake complete"
        );

        debug!("[PULL] Socket initialized");

        Ok(Self {
            base: SocketBase::new(stream, SocketType::Pull, options),
            frames: SmallVec::new(),
        })
    }

    /// Receive a message from a connected PUSH socket.
    ///
    /// When multiple PUSH sockets are connected, messages are received
    /// in a fair-queued manner (in a multi-connection scenario).
    ///
    /// Returns `Ok(Some(msg))` if a message was received, `Ok(None)` if the
    /// connection was closed, or an error.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[PULL] Waiting for message");

        // Read from stream until we have a complete message
        loop {
            // Try to decode frames from buffer
            loop {
                match self.base.decoder.decode(&mut self.base.recv)? {
                    Some(frame) => {
                        if frame.is_command() {
                            if crate::base::is_ping_payload(&frame.payload) {
                                let pong = crate::base::build_pong_frame();
                                self.base.send_buffer.extend_from_slice(&pong);
                                self.base.flush_send_buffer().await?;
                            } else if crate::base::is_pong_payload(&frame.payload) {
                                self.base.note_pong_received();
                            }
                            continue;
                        }
                        let more = frame.more();
                        self.frames.push(frame.payload);

                        if !more {
                            // Complete message received
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[PULL] Received {} frames", msg.len());
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
                trace!("[PULL] Connection closed");
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
            // Continue decoding with new data
        }
    }

    /// Close the socket gracefully.
    pub async fn close(self) -> io::Result<()> {
        trace!("[PULL] Closing socket");
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
impl PullSocket<TcpStream> {
    /// Create a new PULL socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PULL socket from a TCP stream with TCP_NODELAY and custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Configure TCP optimizations including keepalive
        crate::utils::configure_tcp_stream(&stream, &options, "PULL")?;
        Self::with_options(stream, options).await
    }

    /// Connect to a remote PULL socket, storing the endpoint for automatic reconnection.
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
        crate::utils::configure_tcp_stream(&stream, &options, "PULL")?;

        let mut stream = stream;
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Pull,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PULL] Connected to {} (endpoint stored for reconnection)",
            peer_addr
        );

        let endpoint = monocoque_core::endpoint::Endpoint::Tcp(peer_addr);
        Ok(Self {
            base: crate::base::SocketBase::with_endpoint(
                stream,
                SocketType::Pull,
                endpoint,
                options,
            ),
            frames: SmallVec::new(),
        })
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }

    /// Try to reconnect to the stored endpoint.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base.try_reconnect(SocketType::Pull).await
    }

    /// Receive a message with automatic reconnection on EOF or network error.
    ///
    /// If the socket was created with `connect()` and stores an endpoint, this
    /// method loops: on EOF or broken-pipe it clears the stream and calls
    /// `try_reconnect()` (which applies exponential backoff), then retries `recv()`.
    ///
    /// Respects `max_reconnect_attempts` — returns `NotConnected` when exhausted.
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
                    "[PULL] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.recv().await {
                Ok(Some(msg)) => return Ok(Some(msg)),
                // EOF: read_raw() already set stream = None
                Ok(None) => {
                    debug!("[PULL] EOF on recv, will reconnect");
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
                        debug!("[PULL] Connection error on recv ({}), will reconnect", e);
                        self.base.stream = None;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }
}

crate::impl_socket_trait!(PullSocket<S>, SocketType::Pull);
