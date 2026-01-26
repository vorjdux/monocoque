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

use crate::{handshake::perform_handshake_with_timeout, session::SocketType};
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
    pub async fn with_options(
        mut stream: S,
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

        debug!("[SUB] Socket initialized");

        Ok(Self {
            base: SocketBase::new(stream, SocketType::Sub, options),
            frames: SmallVec::new(),
            subscriptions: Vec::new(),
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
        
        let stream = self.base.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;
        let BufResult(result, _) = AsyncWrite::write(stream, IoBytes::new(buf)).await;
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
        
        let stream = self.base.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;
        let BufResult(result, _) = AsyncWrite::write(stream, IoBytes::new(buf)).await;
        result?;
        
        trace!("[SUB] Unsubscription message sent successfully");
        
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
