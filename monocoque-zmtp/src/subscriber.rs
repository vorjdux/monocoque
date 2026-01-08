//! Direct-stream SUB socket implementation
//!
//! This module provides a high-performance SUB socket using direct stream I/O
//! for minimal latency.
//!
//! # SUB Pattern
//!
//! SUB sockets receive messages from PUB sockets and filter them based on
//! subscriptions.

use bytes::Bytes;
use compio::io::AsyncRead;
use compio::net::TcpStream;
use monocoque_core::alloc::IoArena;
use monocoque_core::buffer::SegmentedBuffer;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

use crate::{codec::ZmtpDecoder, config::BufferConfig, handshake::perform_handshake, session::SocketType};

/// Direct-stream SUB socket.
pub struct SubSocket {
    /// Underlying TCP stream
    stream: TcpStream,
    /// ZMTP decoder for decoding frames
    decoder: ZmtpDecoder,
    /// Arena for zero-copy allocation
    arena: IoArena,
    /// Segmented read buffer for incoming data
    recv: SegmentedBuffer,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
    /// List of subscription prefixes (sorted for efficient matching)
    subscriptions: Vec<Bytes>,
    /// Buffer configuration
    config: BufferConfig,
}

impl SubSocket {
    /// Create a new SUB socket from a TCP stream.
    pub async fn new(mut stream: TcpStream) -> io::Result<Self> {
        debug!("[SUB] Creating new direct SUB socket");

        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[SUB] TCP_NODELAY enabled");

        // Perform ZMTP handshake
        debug!("[SUB] Performing ZMTP handshake...");
        let handshake_result = perform_handshake(&mut stream, SocketType::Sub, None)
            .await
            .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[SUB] Handshake complete"
        );

        // Create arena for zero-copy allocation
        let arena = IoArena::new();

        // Create ZMTP decoder
        let decoder = ZmtpDecoder::new();

        // Create buffer config
        let config = BufferConfig::default();

        // Create buffers
        let recv = SegmentedBuffer::new();

        debug!("[SUB] Socket initialized");

        Ok(Self {
            stream,
            decoder,
            arena,
            recv,
            frames: SmallVec::new(),
            subscriptions: Vec::new(),
            config,
        })
    }

    /// Subscribe to messages with the given prefix.
    ///
    /// An empty prefix subscribes to all messages.
    pub fn subscribe(&mut self, prefix: Bytes) {
        trace!("[SUB] Adding subscription: {:?}", prefix);
        if !self.subscriptions.contains(&prefix) {
            self.subscriptions.push(prefix);
            self.subscriptions.sort();
        }
    }

    /// Unsubscribe from messages with the given prefix.
    pub fn unsubscribe(&mut self, prefix: &Bytes) {
        trace!("[SUB] Removing subscription: {:?}", prefix);
        self.subscriptions.retain(|s| s != prefix);
    }

    /// Check if a message matches any subscription.
    fn matches_subscription(&self, msg: &[Bytes]) -> bool {
        // If no subscriptions, nothing matches
        if self.subscriptions.is_empty() {
            return false;
        }

        // Empty subscription matches everything
        if self.subscriptions.iter().any(bytes::Bytes::is_empty) {
            return true;
        }

        // Check if first frame starts with any subscription prefix
        if let Some(first_frame) = msg.first() {
            self.subscriptions
                .iter()
                .any(|sub| first_frame.len() >= sub.len() && first_frame[..sub.len()] == sub[..])
        } else {
            false
        }
    }

    /// Receive a message that matches subscriptions.
    ///
    /// This will keep reading and filtering messages until one matches
    /// the active subscriptions.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        loop {
            trace!("[SUB] Waiting for message");

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
                                trace!("[SUB] Received {} frames", msg.len());

                                // Check if message matches any subscription
                                if self.matches_subscription(&msg) {
                                    return Ok(Some(msg));
                                }
                                trace!("[SUB] Message filtered out (no matching subscription)");
                                // Continue to next message
                                break;
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
                    trace!("[SUB] Connection closed");
                    return Ok(None);
                }

                // Push bytes into segmented recv queue (zero-copy)
                self.recv.push(slab.freeze());
            }
        }
    }
}
