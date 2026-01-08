//! Direct-stream ROUTER socket implementation
//!
//! This module provides a high-performance ROUTER socket using direct stream I/O
//! for minimal latency.
//!
//! # ROUTER Pattern
//!
//! ROUTER sockets receive messages with sender identity and can route replies
//! back to specific senders.

use bytes::{Bytes, BytesMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::alloc::IoArena;
use monocoque_core::alloc::IoBytes;
use smallvec::SmallVec;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, trace};

use crate::codec::{encode_multipart, ZmtpDecoder};
use crate::config::BufferConfig;
use crate::{handshake::perform_handshake, session::SocketType};
use monocoque_core::buffer::SegmentedBuffer;

static PEER_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Direct-stream ROUTER socket.
pub struct RouterSocket {
    /// Underlying TCP stream
    stream: TcpStream,
    /// ZMTP decoder for decoding frames
    decoder: ZmtpDecoder,
    /// Arena for zero-copy allocation
    arena: IoArena,
    /// Segmented read buffer for incoming data
    recv: SegmentedBuffer,
    /// Write buffer for outgoing data (reused to avoid allocations)
    write_buf: BytesMut,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
    /// Peer identity (auto-generated or from handshake)
    peer_identity: Bytes,
    /// Buffer configuration
    config: BufferConfig,
}

impl RouterSocket {
    /// Create a new ROUTER socket from a TCP stream.
    pub async fn new(mut stream: TcpStream) -> io::Result<Self> {
        debug!("[ROUTER] Creating new direct ROUTER socket");

        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[ROUTER] TCP_NODELAY enabled");

        // Perform ZMTP handshake
        debug!("[ROUTER] Performing ZMTP handshake...");
        let handshake_result = perform_handshake(&mut stream, SocketType::Router, None)
            .await
            .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        // Get or generate peer identity
        let peer_identity = if let Some(id) = handshake_result.peer_identity {
            id
        } else {
            // Auto-generate identity using counter
            let peer_id = PEER_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
            Bytes::from(format!("peer-{}", peer_id))
        };

        debug!(
            peer_identity = ?peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[ROUTER] Handshake complete"
        );

        // Create arena for zero-copy allocation
        let arena = IoArena::new();

        // Create ZMTP decoder
        let decoder = ZmtpDecoder::new();

        // Create buffer config
        let config = BufferConfig::default();

        // Create buffers
        let recv = SegmentedBuffer::new();
        let write_buf = BytesMut::with_capacity(config.write_buf_size);

        debug!("[ROUTER] Socket initialized");

        Ok(Self {
            stream,
            decoder,
            arena,
            recv,
            write_buf,
            frames: SmallVec::new(),
            peer_identity,
            config,
        })
    }

    /// Receive a message with sender identity prepended.
    ///
    /// Returns a multipart message where the first frame is the sender identity.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[ROUTER] Waiting for message");

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
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[ROUTER] Received {} frames", msg.len());
                            
                            // Prepend peer identity to the message
                            let mut frames = vec![self.peer_identity.clone()];
                            frames.extend(msg);
                            
                            return Ok(Some(frames));
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
                trace!("[ROUTER] Connection closed");
                return Ok(None);
            }

            // Push bytes into segmented recv queue (zero-copy)
            self.recv.push(slab.freeze());
        }
    }

    /// Send a message.
    ///
    /// For ROUTER sockets, the first frame should be the destination identity,
    /// but since this is a single-peer connection, we just send the frames as-is.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[ROUTER] Sending {} frames", msg.len());

        // Skip the first frame (identity) if present and send the rest
        let frames_to_send = if msg.len() > 1 {
            &msg[1..]
        } else {
            &msg[..]
        };

        // Encode message - reuse write_buf to avoid allocation
        self.write_buf.clear();
        encode_multipart(frames_to_send, &mut self.write_buf);

        // Write to stream using compio
        use compio::buf::BufResult;

        let buf = self.write_buf.split().freeze();
        let BufResult(result, _) = AsyncWrite::write(&mut self.stream, IoBytes::new(buf)).await;
        result?;

        trace!("[ROUTER] Message sent successfully");
        Ok(())
    }
}
