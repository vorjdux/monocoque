//! PULL socket implementation
//!
//! PULL sockets are receive-only endpoints in the pipeline pattern. They receive
//! messages from connected PUSH sockets in a fair-queued manner.
//!
//! # Characteristics
//!
//! - **Receive-only**: Cannot send messages
//! - **Fair-queued**: Receives from all PUSH sockets fairly
//! - **Pipeline pattern**: For receiving tasks from distributors
//! - **No filtering**: All messages are delivered
//!
//! # Use Cases
//!
//! - Task receiver (worker pattern)
//! - Parallel pipeline processing
//! - Work queue consumption

use crate::base::SocketBase;
use crate::{handshake::perform_handshake_with_options, session::SocketType};
use bytes::Bytes;
use compio_io::{AsyncRead, AsyncWrite};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::TcpStream;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

/// PULL socket for receiving messages in a pipeline.
///
/// PULL sockets receive messages from connected PUSH sockets, providing
/// the worker side of the pipeline pattern.
pub struct PullSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
}

impl<S> PullSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new PULL socket from a stream with default buffer configuration.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PULL socket with custom buffer configuration and socket options.
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[PULL] Creating new PULL socket");

        // Perform ZMTP handshake
        debug!("[PULL] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Pull,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PULL] Handshake complete"
        );

        debug!("[PULL] Socket initialized");

        let mut base = SocketBase::new(stream, SocketType::Pull, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
        })
    }

    /// Try to receive a message from the already-buffered input without doing a
    /// kernel read.
    ///
    /// Decodes from bytes already present in the receive buffer. Returns
    /// `Ok(None)` immediately when the buffer is empty rather than suspending.
    /// Use this after `recv()` to drain all messages from a single read batch
    /// before returning to the io_uring submission loop:
    ///
    /// ```rust,no_run
    /// # async fn example(pull: &mut monocoque_zmtp::PullSocket) -> std::io::Result<()> {
    /// // One kernel read may deliver many messages - drain them all before
    /// // going back to the event loop.
    /// if let Some(first) = pull.recv().await? {
    ///     process(first);
    ///     while let Some(msg) = pull.try_recv()? {
    ///         process(msg);
    ///     }
    /// }
    /// # fn process(_: Vec<bytes::Bytes>) {}
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// When a PING heartbeat command is decoded the corresponding PONG is
    /// queued in the send buffer; the next `recv()` call flushes it. For
    /// pure pipeline throughput benchmarks (where heartbeats are inactive) this
    /// is never triggered.
    pub fn try_recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        loop {
            match self.base.process_frame()? {
                crate::base::FrameResult::NeedMore => return Ok(None),
                crate::base::FrameResult::CommandHandled => {
                    // PONG or other response is already in send_buffer;
                    // the next recv() call will flush it.
                }
                crate::base::FrameResult::Data(more, payload) => {
                    self.frames.push(payload);
                    if !more {
                        let msg: Vec<Bytes> = self.frames.drain(..).collect();
                        return Ok(Some(msg));
                    }
                }
            }
        }
    }

    /// Try to receive a message into a caller-provided buffer, without a kernel read.
    ///
    /// The allocation-free counterpart to [`try_recv`](Self::try_recv): on a
    /// complete message the frames are moved into `out` (reusing its capacity) and
    /// `Ok(true)` is returned; when no complete message is buffered it returns
    /// `Ok(false)` and leaves `out` untouched. Partial frames stay in the socket's
    /// accumulator, so it interleaves correctly with [`recv_into`](Self::recv_into)
    /// for multipart messages split across reads.
    pub fn try_recv_into(&mut self, out: &mut Vec<Bytes>) -> io::Result<bool> {
        loop {
            match self.base.process_frame()? {
                crate::base::FrameResult::NeedMore => return Ok(false),
                crate::base::FrameResult::CommandHandled => {}
                crate::base::FrameResult::Data(more, payload) => {
                    self.frames.push(payload);
                    if !more {
                        out.clear();
                        out.extend(self.frames.drain(..));
                        return Ok(true);
                    }
                }
            }
        }
    }

    /// Receive a message from a connected PUSH socket.
    ///
    /// When multiple PUSH sockets are connected, messages are received
    /// in a fair-queued manner (in a multi-connection scenario).
    ///
    /// Returns `Ok(Some(msg))` if a message was received, `Ok(None)` if the
    /// connection was closed, or an error.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        trace!("[PULL] Waiting for message");

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
                            trace!("[PULL] Received {} frames", msg.len());
                            return Ok(Some(msg));
                        }
                    }
                }
            }

            // Need more data - read raw bytes from stream
            let n = self.base.read_raw().await?;
            if n == 0 {
                // EOF - connection closed
                trace!("[PULL] Connection closed");
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
            // Continue decoding with new data
        }
    }

    /// Receive a message into a caller-provided buffer, reusing its allocation.
    ///
    /// Identical to [`recv`](Self::recv) except the message frames are pushed
    /// straight into `out` instead of a freshly allocated `Vec`. The caller keeps
    /// one `Vec` and passes it on every call, so a steady recv loop performs no
    /// per-message heap allocation (the dominant per-message cost at small message
    /// sizes). `out` is cleared on entry.
    ///
    /// Returns `Ok(true)` when a complete message was read into `out`, `Ok(false)`
    /// when the connection was closed.
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
                        // Accumulate in the shared frame buffer so a multipart
                        // message split across reads (or across a try_recv_into)
                        // is reassembled correctly, then move it into `out`,
                        // reusing the caller's allocation.
                        self.frames.push(payload);
                        if !more {
                            out.extend(self.frames.drain(..));
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

    /// Receive a batch of messages with a single `.await`.
    ///
    /// Blocks until at least one message is available (like [`recv`](Self::recv)),
    /// then drains every further message already decoded from the same kernel
    /// read(s) without suspending again. One `read` frequently delivers many
    /// small messages; returning them all from one `.await` amortizes the
    /// per-await overhead that becomes a real fraction of the budget at
    /// multi-million-msg/s rates. It is the receive-side counterpart to
    /// [`PushSocket::send_batch`](crate::push::PushSocket::send_batch).
    ///
    /// Returns `Ok(Some(batch))` with one or more messages (in arrival order),
    /// or `Ok(None)` if the connection was closed before any message arrived.
    pub async fn recv_batch(&mut self) -> io::Result<Option<Vec<Vec<Bytes>>>> {
        let Some(first) = self.recv().await? else {
            return Ok(None);
        };

        let mut batch = Vec::with_capacity(8);
        batch.push(first);

        // Drain everything else already sitting in the receive buffer.
        while let Some(msg) = self.try_recv()? {
            batch.push(msg);
        }

        // A PING may have been decoded mid-drain, queuing a PONG; flush it.
        if !self.base.send_buffer.is_empty() {
            self.base.flush_send_buffer().await?;
        }

        Ok(Some(batch))
    }

    /// Close the socket gracefully by shutting down the underlying stream.
    pub async fn close(mut self) -> io::Result<()> {
        trace!("[PULL] Closing socket");
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
        self.base.set_options(options);
    }
}

// Specialized implementation for TCP streams to enable TCP_NODELAY
impl PullSocket<TcpStream> {
    /// Create a new PULL socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a new PULL socket from a TCP stream with TCP_NODELAY and custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Configure TCP optimizations including keepalive
        crate::utils::configure_tcp_stream(&stream, &options, "PULL")?;
        Self::with_options(stream, options).await
    }

    /// Connect to a remote PULL socket, storing the endpoint for automatic reconnection.
    pub async fn connect(addr: impl monocoque_core::rt::ToSocketAddrs) -> io::Result<Self> {
        Self::connect_with_options(addr, SocketOptions::default()).await
    }

    /// Connect with custom options, storing the endpoint for reconnection.
    pub async fn connect_with_options(
        addr: impl monocoque_core::rt::ToSocketAddrs,
        options: SocketOptions,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        let peer_addr = stream.peer_addr()?;
        crate::utils::configure_tcp_stream(&stream, &options, "PULL")?;

        let mut stream = stream;
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Pull,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[PULL] Connected to {} (endpoint stored for reconnection)",
            peer_addr
        );

        let endpoint = monocoque_core::endpoint::Endpoint::Tcp(peer_addr);
        let mut base =
            crate::base::SocketBase::with_endpoint(stream, SocketType::Pull, endpoint, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            frames: SmallVec::new(),
        })
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }

    /// Try to reconnect to the stored endpoint.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base.try_reconnect(SocketType::Pull).await
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
                    "[PULL] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.recv().await {
                Ok(Some(msg)) => return Ok(Some(msg)),
                // EOF: read_raw() already set stream = None
                Ok(None) => {
                    debug!("[PULL] EOF on recv, will reconnect");
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
                        debug!("[PULL] Connection error on recv ({}), will reconnect", e);
                        self.base.stream = None;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }
}

crate::impl_socket_trait!(PullSocket<S>, SocketType::Pull);
