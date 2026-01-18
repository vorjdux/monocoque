//! Direct-stream SUB socket implementation
//!
//! This module provides a high-performance SUB socket using direct stream I/O
//! for minimal latency.
//!
//! # SUB Pattern
//!
//! SUB sockets receive messages from PUB sockets and filter them based on
//! subscriptions.

use bytes::{Bytes, BytesMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::alloc::IoArena;
use monocoque_core::buffer::SegmentedBuffer;
use monocoque_core::options::SocketOptions;
use monocoque_core::poison::PoisonGuard;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

use crate::{codec::ZmtpDecoder, handshake::perform_handshake_with_timeout, session::SocketType};
use monocoque_core::config::BufferConfig;

/// Direct-stream SUB socket.
pub struct SubSocket<S = TcpStream>
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
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
    /// List of subscription prefixes (sorted for efficient matching)
    subscriptions: Vec<Bytes>,
    /// Buffer configuration
    config: BufferConfig,
    /// Socket options (timeouts, limits, etc.)
    options: SocketOptions,
    /// Connection health flag (true if receive was cancelled mid-operation)
    is_poisoned: bool,
}

impl<S> SubSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new SUB socket from a stream with large buffer configuration (16KB).
    ///
    /// SUB sockets typically receive bulk data from publishers,
    /// so large buffers provide optimal performance. Use `with_config()` for different workloads.
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, BufferConfig::large(), SocketOptions::default()).await
    }

    /// Create a new SUB socket from a stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency pub/sub with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput pub/sub with large messages
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn with_config(stream: S, config: BufferConfig) -> io::Result<Self> {
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new SUB socket with custom buffer configuration and socket options.
    pub async fn with_options(
        mut stream: S,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[SUB] Creating new direct SUB socket");

        // Perform ZMTP handshake
        debug!("[SUB] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Sub,
            None,
            Some(options.handshake_timeout),
        )
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
            options,
            is_poisoned: false,
        })
    }

    /// Subscribe to messages with the given prefix.
    ///
    /// An empty prefix subscribes to all messages.
    /// 
    /// This sends a subscription message to the PUB socket per ZMTP protocol.
    pub async fn subscribe(&mut self, prefix: impl Into<Bytes>) -> io::Result<()> {
        let prefix = prefix.into();
        trace!("[SUB] Adding subscription: {:?}", prefix);
        
        if !self.subscriptions.contains(&prefix) {
            self.subscriptions.push(prefix.clone());
            self.subscriptions.sort();
        }

        // Send subscription message to PUB socket
        // Format: [0x01] [subscription prefix...]
        use compio::buf::BufResult;
        use compio::io::AsyncWrite;
        use monocoque_core::alloc::IoBytes;
        
        let mut sub_msg = BytesMut::with_capacity(prefix.len() + 1);
        sub_msg.extend_from_slice(&[0x01]); // Subscribe command
        sub_msg.extend_from_slice(&prefix);
        
        let buf = sub_msg.freeze();
        trace!("[SUB] Sending subscription message ({} bytes)", buf.len());
        
        let BufResult(result, _) = AsyncWrite::write(&mut self.stream, IoBytes::new(buf)).await;
        result?;
        
        trace!("[SUB] Subscription message sent successfully");
        
        Ok(())
    }

    /// Unsubscribe from messages with the given prefix.
    ///
    /// This sends an unsubscription message to the PUB socket per ZMTP protocol.
    pub async fn unsubscribe(&mut self, prefix: &Bytes) -> io::Result<()> {
        trace!("[SUB] Removing subscription: {:?}", prefix);
        self.subscriptions.retain(|s| s != prefix);

        // Send unsubscription message to PUB socket
        // Format: [0x00] [subscription prefix...]
        use compio::buf::BufResult;
        use compio::io::AsyncWrite;
        use monocoque_core::alloc::IoBytes;
        
        let mut unsub_msg = BytesMut::with_capacity(prefix.len() + 1);
        unsub_msg.extend_from_slice(&[0x00]); // Unsubscribe command
        unsub_msg.extend_from_slice(prefix);
        
        let buf = unsub_msg.freeze();
        trace!("[SUB] Sending unsubscription message ({} bytes)", buf.len());
        
        let BufResult(result, _) = AsyncWrite::write(&mut self.stream, IoBytes::new(buf)).await;
        result?;
        
        trace!("[SUB] Unsubscription message sent successfully");
        
        Ok(())
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
        // Check poison flag first
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket is poisoned from previous incomplete operation",
            ));
        }

        // Create guard to poison socket if we panic or cancel mid-operation
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        'outer: loop {
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
                                // Need to check subscriptions without borrowing self while guard is active
                                let matches = {
                                    let subscriptions = &self.subscriptions;
                                    msg.first().map_or(false, |first_frame| {
                                        subscriptions.is_empty()
                                            || subscriptions
                                                .iter()
                                                .any(|sub| first_frame.starts_with(sub))
                                    })
                                };

                                if matches {
                                    guard.disarm();
                                    return Ok(Some(msg));
                                }
                                trace!("[SUB] Message filtered out (no matching subscription)");
                                // Continue looking for next message in buffer
                                continue 'outer;
                            }
                        }
                        None => {
                            break; // Need more data
                        }
                    }
                }

                // Need more data - read from stream using reused buffer
                use compio::buf::BufResult;

                let slab = self.arena.alloc_mut(self.config.read_buf_size);
                
                // Apply recv timeout from options
                let BufResult(result, slab) = match self.options.recv_timeout {
                    None => {
                        // Blocking mode - no timeout
                        AsyncRead::read(&mut self.stream, slab).await
                    }
                    Some(dur) if dur.is_zero() => {
                        // Non-blocking mode
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "Socket is in non-blocking mode and no data is available",
                        ));
                    }
                    Some(dur) => {
                        // Timed mode - apply timeout
                        use compio::time::timeout;
                        match timeout(dur, AsyncRead::read(&mut self.stream, slab)).await {
                            Ok(result) => result,
                            Err(_) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    format!("Receive operation timed out after {:?}", dur),
                                ));
                            }
                        }
                    }
                };
                
                let n = result?;

                if n == 0 {
                    // EOF
                    trace!("[SUB] Connection closed");
                    guard.disarm();
                    return Ok(None);
                }

                // Push bytes into segmented recv queue (zero-copy)
                self.recv.push(slab.freeze());
                println!("[SUB recv] Pushed {} bytes to buffer, total buffer len={}", n, self.recv.len());
            }
        }
    }

    /// Close the socket gracefully.
    ///
    /// SUB sockets don't send data, so this simply drops the socket.
    /// The linger option is not applicable to SUB sockets.
    pub async fn close(self) -> io::Result<()> {
        trace!("[SUB] Closing socket");
        Ok(())
    }

    /// Get a reference to the socket options.
    #[inline]
    pub fn options(&self) -> &SocketOptions {
        &self.options
    }

    /// Get a mutable reference to the socket options.
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.options
    }

    /// Set socket options (builder-style).
    #[inline]
    pub fn set_options(&mut self, options: SocketOptions) {
        self.options = options;
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl SubSocket<TcpStream> {
    /// Create a new SUB socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_config(stream, BufferConfig::large()).await
    }

    /// Create a new SUB socket from a TCP stream with TCP_NODELAY and custom config.
    pub async fn from_tcp_with_config(stream: TcpStream, config: BufferConfig) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[SUB] TCP_NODELAY enabled");
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new SUB socket from a TCP stream with full configuration.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[SUB] TCP_NODELAY enabled");
        Self::with_options(stream, config, options).await
    }
}
