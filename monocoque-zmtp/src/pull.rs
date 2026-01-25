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
use crate::{handshake::perform_handshake_with_timeout, session::SocketType};
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
    pub async fn with_options(
        mut stream: S,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[PULL] Creating new PULL socket");

        // Perform ZMTP handshake
        debug!("[PULL] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Pull,
            None,
            Some(options.handshake_timeout),
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
}

crate::impl_socket_trait!(PullSocket<S>, SocketType::Pull);
