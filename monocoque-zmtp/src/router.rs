//! Direct-stream ROUTER socket implementation
//!
//! This module provides a high-performance ROUTER socket using direct stream I/O
//! for minimal latency.
//!
//! # ROUTER Pattern
//!
//! ROUTER sockets receive messages with sender identity and can route replies
//! back to specific senders.

use bytes::Bytes;
use compio_io::{AsyncRead, AsyncWrite};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::TcpStream;
use smallvec::SmallVec;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, trace};

use crate::base::SocketBase;
use crate::{handshake::perform_handshake_with_options, session::SocketType};
use monocoque_core::endpoint::Endpoint;

static PEER_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Direct-stream ROUTER socket.
pub struct RouterSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
    /// Peer identity (auto-generated or from handshake)
    peer_identity: Bytes,
    /// When true, sending to an unknown identity returns an error instead of
    /// silently dropping the message (ZMQ_ROUTER_MANDATORY).
    router_mandatory: bool,
}

impl<S> RouterSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new ROUTER socket from a stream using default options.
    ///
    /// ROUTER sockets handle high-throughput workloads with message routing.
    /// Uses default buffer sizes (8KB). For custom configuration, use `with_options()`.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::router::RouterSocket;
    /// # use monocoque_core::rt::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = RouterSocket::new(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new ROUTER socket with custom socket options.
    ///
    /// # Buffer Configuration
    /// - Use `SocketOptions::small()` (4KB) for low-latency with small messages
    /// - Use `SocketOptions::large()` (16KB) for high-throughput with large messages
    /// - Use `SocketOptions::default()` (8KB) for balanced workloads
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::router::RouterSocket;
    /// # use monocoque_core::options::SocketOptions;
    /// # use monocoque_core::rt::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let opts = SocketOptions::large(); // 16KB buffers for throughput
    /// let socket = RouterSocket::with_options(stream, opts).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_options(mut stream: S, mut options: SocketOptions) -> io::Result<Self> {
        debug!("[ROUTER] Creating new direct ROUTER socket");

        // Perform ZMTP handshake
        debug!("[ROUTER] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Router,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        // Determine peer identity (priority order):
        // 1. connect_routing_id (explicitly assigned by ROUTER)
        // 2. peer_identity from handshake (peer's self-reported identity)
        // 3. Auto-generate
        let peer_identity = if let Some(id) = options.connect_routing_id.take() {
            // Use the explicitly assigned identity
            debug!("[ROUTER] Using assigned identity: {:?}", id);
            id
        } else if let Some(id) = handshake_result.peer_identity {
            // Use peer's self-reported identity
            debug!("[ROUTER] Using peer-reported identity: {:?}", id);
            id
        } else {
            // Auto-generate identity using counter
            let peer_id = PEER_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
            let id = Bytes::from(format!("\0peer-{}", peer_id));
            debug!("[ROUTER] Auto-generated identity: {:?}", id);
            id
        };

        debug!(
            peer_identity = ?peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[ROUTER] Handshake complete"
        );

        debug!("[ROUTER] Socket initialized");

        let router_mandatory = options.router_mandatory;
        let mut base = SocketBase::new(stream, SocketType::Router, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
            peer_identity,
            router_mandatory,
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
                match self.base.process_frame()? {
                    crate::base::FrameResult::NeedMore => break,
                    crate::base::FrameResult::CommandHandled => {
                        if !self.base.send_buffer.is_empty() {
                            self.base.flush_send_buffer().await?;
                        }
                    }
                    crate::base::FrameResult::Data(more, payload) => {
                        self.frames.push(payload);
                        if !more {
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[ROUTER] Received {} frames", msg.len());
                            let mut frames = vec![self.peer_identity.clone()];
                            frames.extend(msg);
                            return Ok(Some(frames));
                        }
                    }
                }
            }

            // Need more data - read raw bytes from stream
            let n = self.base.read_raw().await?;
            if n == 0 {
                // EOF - connection closed
                trace!("[ROUTER] Connection closed");
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
            // Continue decoding with new data
        }
    }

    /// Send a message immediately.
    ///
    /// The first frame of `msg` is the routing identity of the destination peer.
    /// For this single-peer implementation, the identity must match the connected
    /// peer's identity or the message is silently dropped (or an error is returned
    /// if `router_mandatory` mode is enabled).
    ///
    /// Encodes and sends the message in a single I/O operation.
    /// For high-throughput scenarios, consider using `send_buffered()` + `flush()`
    /// to batch multiple messages.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[ROUTER] Sending {} frames", msg.len());

        if msg.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ROUTER send: empty message",
            ));
        }

        // First frame is the routing identity. Validate it against our connected peer.
        let identity = &msg[0];
        if *identity != self.peer_identity {
            if self.router_mandatory {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("ROUTER mandatory: no route for identity {:?}", identity),
                ));
            }
            // Non-mandatory: silently drop messages to unknown peers
            trace!(
                "[ROUTER] Dropping message to unknown identity {:?}",
                identity
            );
            return Ok(());
        }

        // Skip the identity frame and send the rest
        let frames_to_send = &msg[1..];

        // Encode message into write_buf (with CURVE encryption if active)
        self.base.encode_message_to_write_buf(frames_to_send)?;

        // Delegate to base for writing
        self.base.write_from_buf().await?;

        trace!("[ROUTER] Message sent successfully");
        Ok(())
    }

    /// Set ROUTER mandatory mode.
    ///
    /// When enabled, sending to an unknown peer identity returns a `NotFound` error
    /// instead of silently dropping the message.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_ROUTER_MANDATORY` (33) option.
    pub fn set_router_mandatory(&mut self, mandatory: bool) {
        self.router_mandatory = mandatory;
        self.base.options.router_mandatory = mandatory;
    }

    /// Send a message to the internal buffer without flushing.
    ///
    /// Use this for batching multiple messages before a single flush.
    /// Call `flush()` to send all buffered messages.
    ///
    /// Like `send()`, the first frame must be the routing identity. If the identity
    /// doesn't match the connected peer, the message is dropped (or an error is
    /// returned in mandatory mode).
    pub fn send_buffered(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        if self.base.hwm_reached() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "Send high water mark reached ({} messages). Flush or drop messages.",
                    self.base.options.send_hwm
                ),
            ));
        }

        trace!("[ROUTER] Buffering {} frames", msg.len());

        if msg.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ROUTER send_buffered: empty message",
            ));
        }

        let frames_to_send = if self.peer_identity.is_empty() {
            msg.as_slice()
        } else {
            // Check routing identity
            let identity = &msg[0];
            if *identity != self.peer_identity {
                if self.router_mandatory {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("ROUTER mandatory: no route for identity {:?}", identity),
                    ));
                }
                trace!(
                    "[ROUTER] Dropping buffered message to unknown identity {:?}",
                    identity
                );
                self.base.buffered_messages += 1;
                return Ok(());
            }
            &msg[1..]
        };
        self.base.encode_message_to_send_buf(frames_to_send)?;
        Ok(())
    }

    /// Flush all buffered messages to the network.
    pub async fn flush(&mut self) -> io::Result<()> {
        trace!("[ROUTER] Flushing {} bytes", self.base.send_buffer.len());
        self.base.flush_send_buffer().await?;
        trace!("[ROUTER] Flush completed");
        Ok(())
    }

    /// Send multiple messages in a single batch (convenience method).
    ///
    /// Each message must have the routing identity as the first frame.
    /// Messages to unknown identities are silently dropped (or cause an error in mandatory mode).
    pub async fn send_batch(&mut self, messages: &[Vec<Bytes>]) -> io::Result<()> {
        trace!("[ROUTER] Batching {} messages", messages.len());

        for msg in messages {
            if msg.is_empty() {
                continue;
            }
            let identity = &msg[0];
            if *identity != self.peer_identity {
                if self.router_mandatory {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("ROUTER mandatory: no route for identity {:?}", identity),
                    ));
                }
                trace!(
                    "[ROUTER] Skipping batch message to unknown identity {:?}",
                    identity
                );
                continue;
            }
            let frames_to_send = &msg[1..];
            self.base.encode_message_to_send_buf(frames_to_send)?;
        }

        self.flush().await
    }

    /// Get the number of bytes currently buffered.
    #[inline]
    pub fn buffered_bytes(&self) -> usize {
        self.base.send_buffer.len()
    }

    /// Close the socket gracefully, respecting the linger timeout.
    ///
    /// This method attempts to flush any buffered send data before closing.
    /// The behavior depends on the `linger` option:
    ///
    /// - `Some(Duration::ZERO)`: Close immediately, discarding buffered data
    /// - `Some(duration)`: Try to flush buffered data within the timeout
    /// - `None`: Block indefinitely until all data is sent (default libzmq behavior)
    pub async fn close(mut self) -> io::Result<()> {
        let linger = self.base.options.linger;

        if self.base.send_buffer.is_empty() {
            trace!("[ROUTER] No buffered data, closing immediately");
            return Ok(());
        }

        trace!(
            "[ROUTER] Closing with {} bytes buffered, linger={:?}",
            self.base.send_buffer.len(),
            linger
        );

        match linger {
            Some(dur) if dur.is_zero() => {
                debug!(
                    "[ROUTER] Linger=0, discarding {} bytes",
                    self.base.send_buffer.len()
                );
                Ok(())
            }
            Some(dur) => {
                use monocoque_core::rt::timeout;
                match timeout(dur, self.flush()).await {
                    Ok(Ok(())) => {
                        debug!("[ROUTER] Successfully flushed before close");
                        Ok(())
                    }
                    Ok(Err(e)) => {
                        debug!("[ROUTER] Flush failed: {}", e);
                        Err(e)
                    }
                    Err(_) => {
                        debug!("[ROUTER] Linger timeout expired, closing anyway");
                        Ok(())
                    }
                }
            }
            None => {
                debug!("[ROUTER] Linger=indefinite, flushing all buffered data");
                self.flush().await
            }
        }
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
        self.base.set_options(options);
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type(&self) -> SocketType {
        SocketType::Router
    }

    /// Get the endpoint this socket is connected/bound to, if available.
    ///
    /// Returns `None` if the socket was created from a raw stream.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub fn last_endpoint(&self) -> Option<&Endpoint> {
        self.base.last_endpoint()
    }

    /// Check if the last received message has more frames coming.
    ///
    /// Returns `true` if there are more frames in the current multipart message.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) option.
    #[inline]
    pub fn has_more(&self) -> bool {
        self.base.has_more()
    }

    /// Get the event state of the socket.
    ///
    /// Returns a bitmask indicating ready-to-receive and ready-to-send states.
    ///
    /// # Returns
    ///
    /// - `1` (POLLIN) - Socket is ready to receive
    /// - `2` (POLLOUT) - Socket is ready to send
    /// - `3` (POLLIN | POLLOUT) - Socket is ready for both
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_EVENTS` (15) option.
    #[inline]
    pub fn events(&self) -> u32 {
        self.base.events()
    }

    /// Get the peer identity.
    ///
    /// Returns the identity of the connected peer.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// This identity is used as the first frame in received messages
    /// and as the routing address in sent messages.
    #[inline]
    pub const fn peer_identity(&self) -> &Bytes {
        &self.peer_identity
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl RouterSocket<TcpStream> {
    /// Create a ROUTER socket from a TCP stream with default options.
    ///
    /// Automatically enables TCP_NODELAY and applies TCP keepalive settings.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::router::RouterSocket;
    /// # use monocoque_core::rt::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = RouterSocket::from_tcp(stream).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a ROUTER socket from a TCP stream with custom socket options.
    ///
    /// Automatically enables TCP_NODELAY and applies TCP keepalive settings from options.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use monocoque_zmtp::router::RouterSocket;
    /// # use monocoque_core::options::SocketOptions;
    /// # use monocoque_core::rt::TcpStream;
    /// # async fn example() -> std::io::Result<()> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let mut opts = SocketOptions::large();
    /// opts.tcp_keepalive = 1;
    /// let socket = RouterSocket::from_tcp_with_options(stream, opts).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Apply TCP-specific configuration
        crate::utils::configure_tcp_stream(&stream, &options, "ROUTER")?;

        Self::with_options(stream, options).await
    }
}

crate::impl_socket_trait!(RouterSocket<S>, SocketType::Router);
