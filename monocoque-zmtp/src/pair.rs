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
use crate::codec::encode_multipart;
use crate::inproc_stream::InprocStream;
use crate::{handshake::perform_handshake_with_timeout, session::SocketType};
use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::config::BufferConfig;
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
        Self::with_options(stream, BufferConfig::default(), SocketOptions::default()).await
    }

    /// Create a new PAIR socket with custom buffer configuration.
    pub async fn with_config(stream: S, config: BufferConfig) -> io::Result<Self> {
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new PAIR socket with custom buffer configuration and socket options.
    pub async fn with_options(
        mut stream: S,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[PAIR] Creating new PAIR socket");

        // Perform ZMTP handshake
        debug!("[PAIR] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Pair,
            None,
            Some(options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PAIR] Handshake complete"
        );

        debug!("[PAIR] Socket initialized");

        Ok(Self {
            base: SocketBase::new(stream, config, options),
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

        // Encode message into write_buf
        self.base.write_buf.clear();
        encode_multipart(&msg, &mut self.base.write_buf);

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
                match self.base.decoder.decode(&mut self.base.recv)? {
                    Some(frame) => {
                        let more = frame.more();
                        self.frames.push(frame.payload);

                        if !more {
                            // Complete message received
                            let msg: Vec<Bytes> = self.frames.drain(..).collect();
                            trace!("[PAIR] Received {} frames", msg.len());
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
                trace!("[PAIR] Connection closed");
                return Ok(None);
            }
            // Continue decoding with new data
        }
    }

    /// Close the socket gracefully.
    pub async fn close(self) -> io::Result<()> {
        trace!("[PAIR] Closing socket");
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

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub fn socket_type(&self) -> SocketType {
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

    /// Connect to a remote PAIR socket.
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
    pub async fn connect(
        addr: impl compio::net::ToSocketAddrsAsync,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Self::from_tcp(stream).await
    }

    /// Create a new PAIR socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_config(stream, BufferConfig::default()).await
    }

    /// Create a new PAIR socket from a TCP stream with TCP_NODELAY and custom config.
    pub async fn from_tcp_with_config(
        stream: TcpStream,
        config: BufferConfig,
    ) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[PAIR] TCP_NODELAY enabled");
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new PAIR socket from a TCP stream with TCP_NODELAY and custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[PAIR] TCP_NODELAY enabled");
        Self::with_options(stream, config, options).await
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
        Self::bind_inproc_with_options(endpoint, BufferConfig::default(), SocketOptions::default())
    }

    /// Bind to an inproc endpoint with custom configuration and options.
    pub fn bind_inproc_with_options(
        endpoint: &str,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
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
            base: SocketBase::with_endpoint(stream, parsed_endpoint, config, options),
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
        Self::connect_inproc_with_options(endpoint, BufferConfig::default(), SocketOptions::default())
    }

    /// Connect to an inproc endpoint with custom configuration and options.
    pub fn connect_inproc_with_options(
        endpoint: &str,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[PAIR] Connecting to inproc endpoint: {}", endpoint);

        // Connect to inproc endpoint
        let tx = monocoque_core::inproc::connect_inproc(endpoint)?;
        
        // For inproc, we need to create a receiver channel
        // The sender sends to the bound endpoint, we receive on our own channel
        let (our_tx, our_rx) = flume::unbounded();
        
        // Register our receiver with the sender
        // This is a bit tricky - we need bidirectional communication
        // For now, create a stream with the connection sender and a new receiver
        let stream = InprocStream::new(tx, our_rx);

        // Parse endpoint for storage
        let parsed_endpoint = Endpoint::parse(endpoint)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

        debug!("[PAIR] Connected to {}", endpoint);

        // For inproc, no handshake needed (same process)
        Ok(Self {
            base: SocketBase::with_endpoint(stream, parsed_endpoint, config, options),
            frames: SmallVec::new(),
        })
    }
}
