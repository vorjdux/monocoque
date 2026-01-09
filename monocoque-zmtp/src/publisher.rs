//! Direct-stream PUB socket implementation
//!
//! This module provides a high-performance PUB socket using direct stream I/O
//! for minimal latency.
//!
//! # PUB Pattern
//!
//! PUB sockets are send-only broadcast sockets.

use bytes::{Bytes, BytesMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::alloc::IoArena;
use monocoque_core::alloc::IoBytes;
use std::io;
use std::sync::Arc;
use tracing::{debug, trace};

use crate::{codec::encode_multipart, handshake::perform_handshake, session::SocketType};
use monocoque_core::config::BufferConfig;

/// Direct-stream PUB socket.
pub struct PubSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Underlying stream (TCP or Unix socket)
    stream: S,
    /// Arena for zero-copy allocation
    #[allow(dead_code)]
    arena: Arc<IoArena>,
    /// Write buffer for outgoing data (reused to avoid allocations)
    write_buf: BytesMut,
    /// Buffer configuration
    #[allow(dead_code)]
    config: BufferConfig,
}

impl<S> PubSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new PUB socket from a stream with large buffer configuration (16KB).
    ///
    /// PUB sockets typically broadcast bulk data to multiple subscribers,
    /// so large buffers provide optimal performance. Use `with_config()` for different workloads.
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_config(stream, BufferConfig::large()).await
    }

    /// Create a new PUB socket from a stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency pub/sub with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput pub/sub with large messages
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn with_config(mut stream: S, config: BufferConfig) -> io::Result<Self> {
        debug!("[PUB] Creating new direct PUB socket");

        // Perform ZMTP handshake
        debug!("[PUB] Performing ZMTP handshake...");
        let handshake_result = perform_handshake(&mut stream, SocketType::Pub, None)
            .await
            .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PUB] Handshake complete"
        );

        // Create arena for zero-copy allocation
        let arena = Arc::new(IoArena::new());

        // Create write buffer
        let write_buf = BytesMut::with_capacity(config.write_buf_size);

        debug!("[PUB] Socket initialized");

        Ok(Self {
            stream,
            arena,
            write_buf,
            config,
        })
    }

    /// Send a message.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[PUB] Sending {} frames", msg.len());

        // Encode message - reuse write_buf to avoid allocation
        self.write_buf.clear();
        encode_multipart(&msg, &mut self.write_buf);

        // Write to stream using compio
        use compio::buf::BufResult;

        let buf = self.write_buf.split().freeze();
        let BufResult(result, _) = AsyncWrite::write(&mut self.stream, IoBytes::new(buf)).await;
        result?;

        trace!("[PUB] Message sent successfully");
        Ok(())
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl PubSocket<TcpStream> {
    /// Create a new PUB socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_config(stream, BufferConfig::large()).await
    }

    /// Create a new PUB socket from a TCP stream with TCP_NODELAY and custom config.
    pub async fn from_tcp_with_config(stream: TcpStream, config: BufferConfig) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[PUB] TCP_NODELAY enabled");
        Self::with_config(stream, config).await
    }
}
