//! Direct-stream SUB socket implementation
//!
//! This module provides a high-performance SUB socket using direct stream I/O
//! for minimal latency.
//!
//! # SUB Pattern
//!
//! SUB sockets receive messages from PUB sockets and filter them based on
//! subscriptions.

use crate::base::SocketBase;
use bytes::{Bytes, BytesMut};
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::options::SocketOptions;
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

use crate::{handshake::perform_handshake_with_options, session::SocketType};
use monocoque_core::endpoint::Endpoint;

/// Direct-stream SUB socket.
pub struct SubSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure (stream, buffers, options)
    base: SocketBase<S>,
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
    /// List of subscription prefixes (sorted for efficient matching)
    subscriptions: Vec<Bytes>,
}

impl<S> SubSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new SUB socket from a stream with large buffer configuration (16KB).
    ///
    /// SUB sockets typically receive bulk data from publishers,
    /// so large buffers provide optimal performance. Use `with_options()` for different configurations.
    ///
    /// Works with both TCP and Unix domain sockets.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new SUB socket with custom buffer configuration and socket options.
    ///
    /// Subscriptions and unsubscriptions set via [`SocketOptions::with_subscribe`] /
    /// [`SocketOptions::with_unsubscribe`] are sent to the peer immediately after
    /// the handshake completes, before this function returns.
    pub async fn with_options(mut stream: S, mut options: SocketOptions) -> io::Result<Self> {
        debug!("[SUB] Creating new direct SUB socket");

        // Perform ZMTP handshake
        debug!("[SUB] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Sub,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[SUB] Handshake complete"
        );

        // Extract subscription lists before moving options into SocketBase.
        let initial_subs = std::mem::take(&mut options.subscriptions);
        let initial_unsubs = std::mem::take(&mut options.unsubscriptions);

        let mut socket = Self {
            base: SocketBase::new(stream, SocketType::Sub, options),
            frames: SmallVec::new(),
            subscriptions: Vec::new(),
        };

        // Apply subscriptions/unsubscriptions declared in options.
        for prefix in initial_subs {
            socket.subscribe(prefix).await?;
        }
        for prefix in initial_unsubs {
            socket.unsubscribe(&prefix).await?;
        }

        debug!("[SUB] Socket initialized");

        Ok(socket)
    }

    /// Subscribe to messages with the given prefix.
    ///
    /// An empty prefix subscribes to all messages.
    ///
    /// This sends a subscription message to the PUB socket as a ZMTP frame.
    pub async fn subscribe(&mut self, prefix: impl Into<Bytes>) -> io::Result<()> {
        let prefix = prefix.into();
        trace!("[SUB] Adding subscription: {:?}", prefix);

        if !self.subscriptions.contains(&prefix) {
            self.subscriptions.push(prefix.clone());
            self.subscriptions.sort();
        }

        self.send_sub_event(0x01, &prefix).await
    }

    /// Unsubscribe from messages with the given prefix.
    ///
    /// This sends an unsubscription message to the PUB socket as a ZMTP frame.
    pub async fn unsubscribe(&mut self, prefix: &Bytes) -> io::Result<()> {
        trace!("[SUB] Removing subscription: {:?}", prefix);
        self.subscriptions.retain(|s| s != prefix);

        self.send_sub_event(0x00, prefix).await
    }

    /// Encode and send a subscription/unsubscription event as a ZMTP frame.
    ///
    /// Wire format: [flags][len][cmd: 0x01|0x00][prefix...]
    /// Using ZMTP framing ensures the PUB's subscription_reader can split
    /// consecutive messages even when they arrive in the same TCP segment.
    async fn send_sub_event(&mut self, cmd: u8, prefix: &[u8]) -> io::Result<()> {
        use compio::buf::BufResult;
        use compio::io::AsyncWriteExt;
        // Build payload: [cmd][prefix]
        let mut payload = BytesMut::with_capacity(1 + prefix.len());
        payload.extend_from_slice(&[cmd]);
        payload.extend_from_slice(prefix);
        let payload = payload.freeze();

        // ZMTP-frame it (single-frame message)
        let mut wire = BytesMut::new();
        crate::codec::encode_multipart(&[payload], &mut wire);
        let wire = wire.freeze();

        trace!("[SUB] Sending subscription event ({} wire bytes)", wire.len());

        let stream = self.base.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;
        let data = wire.to_vec();
        let BufResult(result, _) = stream.write_all(data).await;
        result?;

        trace!("[SUB] Subscription event sent successfully");
        Ok(())
    }

    // Subscription matching logic is implemented inline in recv() for better performance

    /// Receive a message that matches subscriptions.
    ///
    /// This will keep reading and filtering messages until one matches
    /// the active subscriptions.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        'outer: loop {
            trace!("[SUB] Waiting for message");

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
                                trace!("[SUB] Received {} frames", msg.len());

                                // Check if message matches any subscription
                                let matches = {
                                    let subscriptions = &self.subscriptions;
                                    msg.first().is_some_and(|first_frame| {
                                        subscriptions.is_empty()
                                            || subscriptions
                                                .iter()
                                                .any(|sub| first_frame.starts_with(sub))
                                    })
                                };

                                if matches {
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

                // Need more data - read raw bytes from stream
                let n = self.base.read_raw().await?;
                if n == 0 {
                    // EOF
                    trace!("[SUB] Connection closed");
                    return Ok(None);
                }
                // Continue decoding with new data
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
        SocketType::Sub
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
impl SubSocket<TcpStream> {
    /// Create a new SUB socket from a TCP stream with TCP_NODELAY enabled.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Self::from_tcp_with_options(stream, SocketOptions::default()).await
    }

    /// Create a new SUB socket from a TCP stream with full configuration.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        // Configure TCP optimizations including keepalive
        crate::utils::configure_tcp_stream(&stream, &options, "SUB")?;
        Self::with_options(stream, options).await
    }
}

crate::impl_socket_trait!(SubSocket<S>, SocketType::Sub);
