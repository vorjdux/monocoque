//! PAIR socket implementation
//!
//! PAIR sockets are exclusive peer-to-peer sockets that connect exactly two endpoints.
//! They provide bidirectional communication without routing or filtering.
//!
//! # Characteristics
//!
//! - **Exclusive**: Only connects to one peer at a time
//! - **Bidirectional**: Can both send and receive messages
//! - **No routing**: Messages go directly between the pair
//! - **No filtering**: All messages are delivered
//!
//! # Use Cases
//!
//! - Connecting two threads in a process
//! - Exclusive communication between two services
//! - Testing and prototyping

use crate::base::SocketBase;
use crate::inproc_stream::InprocStream;
use crate::{handshake::perform_handshake_with_options, session::SocketType};
use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::endpoint::Endpoint;
use monocoque_core::options::SocketOptions;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

/// PAIR socket for exclusive peer-to-peer communication.
///
/// PAIR sockets connect exactly two endpoints and provide bidirectional
/// message passing without any routing or filtering logic.
pub struct PairSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
}

impl<S> PairSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new PAIR socket from a stream with default buffer configuration.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PAIR socket with custom buffer configuration and socket options.
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[PAIR] Creating new PAIR socket");

        // Perform ZMTP handshake
        debug!("[PAIR] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Pair,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PAIR] Handshake complete"
        );

        debug!("[PAIR] Socket initialized");

        let mut base = SocketBase::new(stream, SocketType::Pair, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
        })
    }

    /// Send a message to the paired socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket is poisoned, disconnected, or if the write fails.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[PAIR] Sending {} frames", msg.len());

        // Encode message into write_buf (with CURVE encryption if active)
        self.base.encode_message_to_write_buf(&msg)?;

        // Delegate to base for writing
        self.base.write_from_buf().await?;

        trace!("[PAIR] Message sent successfully");
        Ok(())
    }

    /// Receive a message from the paired socket.
    ///
    /// Returns `Ok(Some(msg))` if a message was received, `Ok(None)` if the
    /// connection was closed, or an error.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[PAIR] Waiting for message");

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
                        continue;
                    }
                    crate::base::FrameResult::Data(more, payload) => {
                        self.frames.push(payload);
                        if !more {
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[PAIR] Received {} frames", msg.len());
                            return Ok(Some(msg));
                        }
                    }
                }
            }

            // Need more data - read raw bytes from stream
            let n = self.base.read_raw().await?;
            if n == 0 {
                // EOF - connection closed
                trace!("[PAIR] Connection closed");
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
            // Continue decoding with new data
        }
    }

    /// Close the socket gracefully by shutting down the underlying stream.
    pub async fn close(mut self) -> io::Result<()> {
        trace!("[PAIR] Closing socket");
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

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type(&self) -> SocketType {
        SocketType::Pair
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
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl PairSocket<TcpStream> {
    /// Bind to an address and accept the first connection.
    ///
    /// PAIR sockets form an exclusive pair with exactly one peer.
    ///
    /// # Returns
    ///
    /// A tuple of `(listener, socket)` where:
    /// - `listener` can be used to accept additional connections if needed
    /// - `socket` is ready to send/receive with the first peer
    ///
    /// # Example
    ///
    /// ```no_run
    /// use monocoque_zmtp::pair::PairSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let (listener, mut socket) = PairSocket::bind("127.0.0.1:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn bind(
        addr: impl compio::net::ToSocketAddrsAsync,
    ) -> io::Result<(compio::net::TcpListener, Self)> {
        let listener = compio::net::TcpListener::bind(addr).await?;
        let (stream, _) = listener.accept().await?;
        let socket = Self::from_tcp(stream).await?;
        Ok((listener, socket))
    }

    /// Connect to a remote PAIR socket, storing the endpoint for automatic reconnection.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use monocoque_zmtp::pair::PairSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut socket = PairSocket::connect("127.0.0.1:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
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
        crate::utils::configure_tcp_stream(&stream, &options, "PAIR")?;

        let mut stream = stream;
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Pair,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PAIR] Connected to {} (endpoint stored for reconnection)",
            peer_addr
        );

        let endpoint = monocoque_core::endpoint::Endpoint::Tcp(peer_addr);
        let mut base =
            crate::base::SocketBase::with_endpoint(stream, SocketType::Pair, endpoint, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
        })
    }

    /// Create a new PAIR socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PAIR socket from a TCP stream with TCP_NODELAY and custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Configure TCP optimizations including keepalive
        crate::utils::configure_tcp_stream(&stream, &options, "PAIR")?;
        Self::with_options(stream, options).await
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }

    /// Try to reconnect to the stored endpoint.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base.try_reconnect(SocketType::Pair).await
    }

    /// Receive a message with automatic reconnection on EOF or network error.
    ///
    /// If the socket was created with `connect()` and stores an endpoint, this
    /// method loops: on EOF or broken-pipe it clears the stream and calls
    /// `try_reconnect()` (which applies exponential backoff), then retries `recv()`.
    ///
    /// Respects `max_reconnect_attempts`  -  returns `NotConnected` when exhausted.
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
                    "[PAIR] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.recv().await {
                Ok(Some(msg)) => return Ok(Some(msg)),
                // EOF: read_raw() already set stream = None
                Ok(None) => {
                    debug!("[PAIR] EOF on recv, will reconnect");
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
                        debug!("[PAIR] Connection error on recv ({}), will reconnect", e);
                        self.base.stream = None;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
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
                    "[PAIR] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.send(msg.clone()).await {
                Ok(()) => return Ok(()),
                Err(_) if self.base.stream.is_none() => {
                    // write_from_buf set stream = None → network error, retry
                    debug!("[PAIR] Send failed (stream lost), will reconnect");
                }
                Err(e) => return Err(e),
            }
        }
    }
}

// Specialized implementation for Inproc streams
impl PairSocket<InprocStream> {
    /// Bind to an inproc endpoint.
    ///
    /// Creates a new inproc endpoint that other sockets can connect to.
    /// Inproc endpoints must be bound before they can be connected to.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Inproc URI (e.g., "inproc://my-endpoint")
    ///
    /// # Example
    ///
    /// ```no_run
    /// use monocoque_zmtp::pair::PairSocket;
    ///
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let socket = PairSocket::bind_inproc("inproc://my-pair")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn bind_inproc(endpoint: &str) -> io::Result<Self> {
        Self::bind_inproc_with_options(endpoint, SocketOptions::default())
    }

    /// Bind to an inproc endpoint with custom configuration and options.
    pub fn bind_inproc_with_options(endpoint: &str, options: SocketOptions) -> io::Result<Self> {
        debug!("[PAIR] Binding to inproc endpoint: {}", endpoint);

        // Bind to inproc endpoint
        let (tx, rx) = monocoque_core::inproc::bind_inproc(endpoint)?;
        let stream = InprocStream::new(tx, rx);

        // Parse endpoint for storage
        let parsed_endpoint = Endpoint::parse(endpoint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

        debug!("[PAIR] Bound to {}", endpoint);

        // For inproc, no handshake needed (same process)
        Ok(Self {
            base: SocketBase::with_endpoint(stream, SocketType::Pair, parsed_endpoint, options),
            frames: SmallVec::new(),
        })
    }

    /// Connect to an inproc endpoint.
    ///
    /// Connects to a previously bound inproc endpoint.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Inproc URI (e.g., "inproc://my-endpoint")
    ///
    /// # Example
    ///
    /// ```no_run
    /// use monocoque_zmtp::pair::PairSocket;
    ///
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let socket = PairSocket::connect_inproc("inproc://my-pair")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn connect_inproc(endpoint: &str) -> io::Result<Self> {
        Self::connect_inproc_with_options(endpoint, SocketOptions::default())
    }

    /// Connect to an inproc endpoint with custom configuration and options.
    pub fn connect_inproc_with_options(endpoint: &str, options: SocketOptions) -> io::Result<Self> {
        debug!("[PAIR] Connecting to inproc endpoint: {}", endpoint);

        // connect_inproc_bidi returns (to_server_tx, from_server_rx) so we can
        // both send to the server and receive replies from it. The server must
        // have been bound with bind_inproc_bidi.
        let (tx, rx) = monocoque_core::inproc::connect_inproc_bidi(endpoint)?;
        let stream = InprocStream::new(tx, rx);

        // Parse endpoint for storage
        let parsed_endpoint = Endpoint::parse(endpoint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

        debug!("[PAIR] Connected to {}", endpoint);

        // For inproc, no handshake needed (same process)
        Ok(Self {
            base: SocketBase::with_endpoint(stream, SocketType::Pair, parsed_endpoint, options),
            frames: SmallVec::new(),
        })
    }
}

crate::impl_socket_trait!(PairSocket<S>, SocketType::Pair);
