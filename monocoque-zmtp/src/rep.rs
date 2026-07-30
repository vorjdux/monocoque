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

use bytes::Bytes;
use compio_io::{AsyncRead, AsyncWrite};
use monocoque_core::rt::TcpStream;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

use crate::base::SocketBase;
use crate::{handshake::perform_handshake_with_options, session::SocketType};
use monocoque_core::endpoint::Endpoint;
use monocoque_core::options::SocketOptions;

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
/// use monocoque_core::rt::TcpStream;
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
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
    /// The routing envelope of the last received request: every frame up to and
    /// including the first empty delimiter. Stripped from what `recv` returns and
    /// re-prepended by `send` so the reply routes back over the ZMTP REQ/REP
    /// contract (this is what makes libzmq REQ interop work).
    request_envelope: Vec<Bytes>,
    /// Current state of the REP state machine
    state: RepState,
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
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new REP socket from a stream with custom buffer configuration and socket options.
    ///
    /// This provides full control over buffer sizes and timeouts.
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[REP] Creating new direct REP socket");

        // Perform ZMTP handshake with timeout
        debug!("[REP] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Rep,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[REP] Handshake complete"
        );

        debug!("[REP] Socket initialized");

        let mut base = SocketBase::new(stream, SocketType::Rep, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
            request_envelope: Vec::new(),
            state: RepState::AwaitingRequest,
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
                            let mut body = Vec::new();
                            self.split_request_envelope(&mut body);
                            trace!(
                                "[REP] Received {} body frames (envelope {} frames)",
                                body.len(),
                                self.request_envelope.len()
                            );
                            self.state = RepState::ReadyToReply;
                            return Ok(Some(body));
                        }
                    }
                }
            }

            // Need more data - read raw bytes from stream
            let n = self.base.read_raw().await?;
            if n == 0 {
                // EOF - connection closed
                trace!("[REP] Connection closed");
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
            // Continue decoding with new data
        }
    }

    /// Split the just-assembled request in `self.frames` into the routing
    /// envelope (kept in `self.request_envelope` for `send` to re-prepend) and
    /// the body (moved into `out`). Every frame up to and including the first
    /// empty delimiter is the envelope; a non-REQ peer with no delimiter yields
    /// an empty envelope and an all-body message. Both target buffers are reused.
    fn split_request_envelope(&mut self, out: &mut Vec<Bytes>) {
        out.clear();
        self.request_envelope.clear();
        if let Some(delim) = self.frames.iter().position(bytes::Bytes::is_empty) {
            out.extend(self.frames.drain(delim + 1..));
            self.request_envelope.extend(self.frames.drain(..));
        } else {
            out.extend(self.frames.drain(..));
        }
        self.frames.clear();
    }

    /// Receive a request body into a caller-provided buffer, reusing its
    /// allocation.
    ///
    /// Allocation-free counterpart to [`recv`](Self::recv): the routing envelope
    /// is stashed for the reply and the body is moved into `out` (cleared on
    /// entry). Returns `Ok(true)` on a complete request, `Ok(false)` on EOF.
    pub async fn recv_into(&mut self, out: &mut Vec<Bytes>) -> io::Result<bool> {
        out.clear();
        loop {
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
                            self.split_request_envelope(out);
                            self.state = RepState::ReadyToReply;
                            return Ok(true);
                        }
                    }
                }
            }

            let n = self.base.read_raw().await?;
            if n == 0 {
                return Ok(false);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
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

        // Re-prepend the request's routing envelope (through the empty delimiter)
        // so the reply routes back to the REQ peer over the ZMTP REQ/REP contract.
        let reply = if self.request_envelope.is_empty() {
            msg
        } else {
            let mut reply = Vec::with_capacity(self.request_envelope.len() + msg.len());
            reply.append(&mut self.request_envelope);
            reply.extend(msg);
            reply
        };

        // Coalesce / vector / copy-and-write as appropriate.
        self.base.send_message(&reply).await?;

        // Transition back to awaiting request
        self.state = RepState::AwaitingRequest;

        trace!("[REP] Reply sent successfully");
        Ok(())
    }

    /// Close the socket gracefully.
    ///
    /// REP sockets send immediately (no buffering), so this simply drops the socket.
    /// The linger option is not applicable to REP sockets.
    pub async fn close(self) -> io::Result<()> {
        trace!("[REP] Closing socket");
        Ok(())
    }

    /// Get the current socket options.
    pub const fn options(&self) -> &SocketOptions {
        &self.base.options
    }

    /// Get a mutable reference to the socket options.
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.base.options
    }

    /// Set socket options.
    pub fn set_options(&mut self, options: SocketOptions) {
        self.base.set_options(options);
    }

    /// Get the current state of the REP socket.
    ///
    /// This is primarily for debugging and testing.
    pub const fn state(&self) -> RepState {
        self.state
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type(&self) -> SocketType {
        SocketType::Rep
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rep_state_machine() {
        use bytes::Bytes;
        use monocoque_core::rt::TcpListener;

        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async {
                // Create a pair of connected sockets
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();

                // Spawn client that will connect and send request
                let client_task = monocoque_core::rt::spawn(async move {
                    monocoque_core::rt::sleep(std::time::Duration::from_millis(10)).await;
                    let stream = monocoque_core::rt::TcpStream::connect(addr).await.unwrap();
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
                monocoque_core::rt::join(client_task).await;
            });
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl RepSocket<TcpStream> {
    /// Create a new REP socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a new REP socket from a TCP stream with TCP_NODELAY and custom config.

    /// Create a new REP socket from a TCP stream with TCP_NODELAY and custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Configure TCP optimizations including keepalive
        crate::utils::configure_tcp_stream(&stream, &options, "REP")?;
        Self::with_options(stream, options).await
    }
}

crate::impl_socket_trait!(RepSocket<S>, SocketType::Rep);
