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
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

use crate::{
    codec::{encode_multipart, ZmtpDecoder},
    handshake::perform_handshake,
    session::SocketType,
};
use monocoque_core::config::BufferConfig;

/// Direct-stream DEALER socket.
pub struct DealerSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Underlying stream (TCP or Unix socket)
    stream: S,
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
        Self::with_config(stream, BufferConfig::large()).await
    }

    /// Create a new DEALER socket from a stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput with large messages
    /// - Use `BufferConfig::custom(read, write)` for fine-grained control
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn with_config(mut stream: S, config: BufferConfig) -> io::Result<Self> {
        debug!("[DEALER] Creating new direct DEALER socket");

        // Perform ZMTP handshake
        debug!("[DEALER] Performing ZMTP handshake...");
        let handshake_result = perform_handshake(&mut stream, SocketType::Dealer, None)
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
            stream,
            decoder,
            arena,
            recv,
            write_buf,
            frames: SmallVec::new(),
            config,
        })
    }

    /// Receive a message.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[DEALER] Waiting for message");

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
            let BufResult(result, slab) = AsyncRead::read(&mut self.stream, slab).await;
            let n = result?;

            if n == 0 {
                // EOF
                trace!("[DEALER] Connection closed");
                return Ok(None);
            }

            // Push bytes into segmented recv queue (zero-copy)
            self.recv.push(slab.freeze());
        }
    }

    /// Send a message.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[DEALER] Sending {} frames", msg.len());

        // Encode message - reuse write_buf to avoid allocation
        self.write_buf.clear();
        encode_multipart(&msg, &mut self.write_buf);

        // Write to stream using compio
        use compio::buf::BufResult;

        let buf = self.write_buf.split().freeze();
        let BufResult(result, _) = AsyncWrite::write(&mut self.stream, IoBytes::new(buf)).await;
        result?;

        trace!("[DEALER] Message sent successfully");
        Ok(())
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
        Self::with_config(stream, config).await
    }
}
