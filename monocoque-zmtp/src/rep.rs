//! Direct-stream REP socket implementation
//!
//! This module provides a high-performance REP socket using direct stream I/O
//! for minimal latency.
//!
//! # REP State Machine
//!
//! REP sockets follow a strict request-reply pattern:
//! - Start in `AwaitingRequest` state
//! - Transition to `ReadyToReply` after receiving a request
//! - Transition back to `AwaitingRequest` after sending a reply
//!
//! Attempting to send before receiving, or receive before sending will return an error.

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

/// REP socket state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepState {
    /// Awaiting a request from the client
    AwaitingRequest,
    /// Received a request, ready to send reply
    ReadyToReply,
}

/// Direct-stream REP socket.
///
/// This implementation provides the REP (reply) socket pattern with minimal latency
/// using direct stream I/O with the compio runtime.
///
/// # State Machine
///
/// The REP socket enforces the request-reply pattern:
/// 1. Start in `AwaitingRequest` - can only call `recv()`
/// 2. After `recv()`, transition to `ReadyToReply` - can only call `send()`
/// 3. After `send()`, transition back to `AwaitingRequest`
///
/// # Performance
///
/// - Direct I/O with buffer reuse
/// - `TCP_NODELAY` enabled
/// - ~10µs latency per round-trip
/// - Zero-copy where possible
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::rep::RepSocket;
/// use compio::net::TcpStream;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = TcpStream::connect("127.0.0.1:5555").await?;
/// let mut socket = RepSocket::new(stream).await?;
///
/// // Echo server loop
/// loop {
///     if let Some(request) = socket.recv().await? {
///         socket.send(request).await?;
///     } else {
///         break; // Connection closed
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct RepSocket<S = TcpStream>
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
    frames: SmallVec<[Bytes; 4]>,
    /// Current state of the REP state machine
    state: RepState,
    /// Buffer configuration
    config: BufferConfig,
}

impl<S> RepSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new REP socket from a stream.
    ///
    /// This performs the ZMTP handshake and initializes the socket.
    /// Works with both TCP and Unix domain sockets.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Handshake fails
    /// - Connection is closed during handshake
    pub async fn new(stream: S) -> io::Result<Self> {
        // REP sockets typically handle low-latency RPC with small messages
        Self::with_config(stream, BufferConfig::small()).await
    }

    /// Create a new REP socket from a stream with custom buffer configuration.
    ///
    /// # Buffer Configuration
    /// - Use `BufferConfig::small()` (4KB) for low-latency request/reply with small messages
    /// - Use `BufferConfig::large()` (16KB) for high-throughput with large messages
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn with_config(mut stream: S, config: BufferConfig) -> io::Result<Self> {
        debug!("[REP] Creating new direct REP socket");

        // Perform ZMTP handshake
        debug!("[REP] Performing ZMTP handshake...");
        let handshake_result = perform_handshake(&mut stream, SocketType::Rep, None)
            .await
            .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[REP] Handshake complete"
        );

        // Create arena for zero-copy allocation
        let arena = IoArena::new();

        // Create ZMTP decoder
        let decoder = ZmtpDecoder::new();

        // Create buffers
        let recv = SegmentedBuffer::new();
        let write_buf = BytesMut::with_capacity(config.write_buf_size);

        debug!("[REP] Socket initialized");

        Ok(Self {
            stream,
            decoder,
            arena,
            recv,
            write_buf,
            frames: SmallVec::new(),
            state: RepState::AwaitingRequest,
            config,
        })
    }

    /// Receive a request message.
    ///
    /// This blocks until a request is received. You must call this before
    /// calling `send()`.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(msg))` - Received a multipart message
    /// - `Ok(None)` - Connection closed gracefully
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called while in `ReadyToReply` state (must call `send()` first)
    /// - I/O error occurs during receive
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::rep::RepSocket;
    /// # async fn example(socket: &mut RepSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// if let Some(request) = socket.recv().await? {
    ///     println!("Got {} frames", request.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // Check state machine
        if self.state != RepState::AwaitingRequest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot recv while in ReadyToReply state - must call send() first",
            ));
        }

        trace!("[REP] Waiting for request");

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
                            trace!("[REP] Received {} frames", msg.len());
                            self.state = RepState::ReadyToReply;
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
                trace!("[REP] Connection closed");
                return Ok(None);
            }

            // Push bytes into segmented recv queue (zero-copy)
            self.recv.push(slab.freeze());
        }
    }

    /// Send a reply message.
    ///
    /// This sends a reply to the previously received request. You must call
    /// `recv()` before calling this.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called while awaiting request (must call `recv()` first)
    /// - I/O error occurs during send
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::rep::RepSocket;
    /// # use bytes::Bytes;
    /// # async fn example(socket: &mut RepSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// socket.send(vec![Bytes::from("REPLY")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        // Check state machine
        if self.state != RepState::ReadyToReply {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cannot send while awaiting request - must call recv() first",
            ));
        }

        trace!("[REP] Sending {} frames", msg.len());

        // Encode message - reuse write_buf to avoid allocation
        self.write_buf.clear();
        encode_multipart(&msg, &mut self.write_buf);

        // Write to stream using compio
        use compio::buf::BufResult;

        let buf = self.write_buf.split().freeze();
        let BufResult(result, _) = AsyncWrite::write(&mut self.stream, IoBytes::new(buf)).await;
        result?;

        // Transition back to awaiting request
        self.state = RepState::AwaitingRequest;

        trace!("[REP] Reply sent successfully");
        Ok(())
    }

    /// Get the current state of the REP socket.
    ///
    /// This is primarily for debugging and testing.
    pub const fn state(&self) -> RepState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rep_state_machine() {
        use bytes::Bytes;
        use compio::net::TcpListener;

        compio::runtime::Runtime::new().unwrap().block_on(async {
            // Create a pair of connected sockets
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            // Spawn client that will connect and send request
            let client_task = compio::runtime::spawn(async move {
                compio::time::sleep(std::time::Duration::from_millis(10)).await;
                let stream = compio::net::TcpStream::connect(addr).await.unwrap();
                let mut req = crate::req::ReqSocket::new(stream).await.unwrap();

                // Send request
                req.send(vec![Bytes::from("test")]).await.unwrap();

                // Wait for and verify reply
                let reply = req.recv().await.unwrap();
                assert!(reply.is_some());

                req
            });

            let (server_stream, _) = listener.accept().await.unwrap();
            let mut rep = RepSocket::new(server_stream).await.unwrap();

            // Initial state
            assert_eq!(rep.state(), RepState::AwaitingRequest);

            // Receive should transition to ReadyToReply
            let msg = rep.recv().await.unwrap();
            assert!(msg.is_some());
            assert_eq!(rep.state(), RepState::ReadyToReply);

            // Send reply should transition back to AwaitingRequest
            rep.send(msg.unwrap()).await.unwrap();
            assert_eq!(rep.state(), RepState::AwaitingRequest);

            // Wait for client
            client_task.await;
        });
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl RepSocket<TcpStream> {
    /// Create a new REP socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_config(stream, BufferConfig::small()).await
    }

    /// Create a new REP socket from a TCP stream with TCP_NODELAY and custom config.
    pub async fn from_tcp_with_config(
        stream: TcpStream,
        config: BufferConfig,
    ) -> io::Result<Self> {
        // Enable TCP_NODELAY for low latency
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[REP] TCP_NODELAY enabled");
        Self::with_config(stream, config).await
    }
}
