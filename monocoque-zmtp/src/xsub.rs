//! XSUB (Extended Subscriber) socket implementation
//!
//! XSUB extends SUB by sending subscription messages upstream to publishers,
//! enabling subscription forwarding in message brokers and dynamic subscription
//! management.
//!
//! # Use Cases
//!
//! - **Message brokers**: Forward subscriptions from frontend to backend
//! - **Cascading pub/sub**: Build subscription trees across network boundaries
//! - **Dynamic subscriptions**: Programmatically manage topic interests
//!
//! # Pattern
//!
//! ```text
//! XSUB ──subscribe("topic.a")──> Publisher
//!      <──────data("topic.a")───
//! XSUB ──subscribe("topic.b")──> Publisher
//!      <──────data("topic.b")───
//! ```

use crate::base::SocketBase;
use bytes::Bytes;
use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use monocoque_core::endpoint::Endpoint;
use monocoque_core::options::SocketOptions;
use monocoque_core::subscription::{SubscriptionEvent, SubscriptionTrie};
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

use crate::handshake::perform_handshake_with_options;
use crate::session::SocketType;

/// XSUB (Extended Subscriber) socket.
///
/// Receives data messages and can send subscription messages upstream.
///
/// # Features
///
/// - **Dynamic subscriptions**: Subscribe/unsubscribe at runtime
/// - **Subscription forwarding**: Forward subscriptions in proxies
/// - **Verbose unsubscribe**: Optionally send explicit unsubscribe messages
///
/// # Examples
///
/// ```no_run
/// use monocoque_zmtp::xsub::XSubSocket;
/// use bytes::Bytes;
///
/// #[compio::main]
/// async fn main() -> std::io::Result<()> {
///     let mut xsub = XSubSocket::connect("127.0.0.1:5555").await?;
///     
///     // Subscribe to topics
///     xsub.subscribe("topic.").await?;
///
///     // Receive messages
///     if let Some(msg) = xsub.recv().await? {
///         println!("Received: {:?}", msg);
///     }
///
///     // Unsubscribe
///     xsub.unsubscribe("topic.").await?;
///     
///     Ok(())
/// }
/// ```
pub struct XSubSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Base socket infrastructure
    base: SocketBase<S>,
    /// Local subscription tracking (XSUB manages subscriptions locally)
    subscriptions: SubscriptionTrie,
}

impl<S> XSubSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new XSUB socket from a stream.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, SocketOptions::default()).await
    }

    /// Create a new XSUB socket with custom configuration and options.
    pub async fn with_options(mut stream: S, options: SocketOptions) -> io::Result<Self> {
        debug!("[XSUB] Creating new XSUB socket");

        // Perform ZMTP handshake
        debug!("[XSUB] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Xsub,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[XSUB] Handshake complete"
        );

        let mut base = SocketBase::new(stream, SocketType::Xsub, options);
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            subscriptions: SubscriptionTrie::new(),
        })
    }

    /// Subscribe to messages with the given prefix.
    ///
    /// Sends a subscription message upstream to the publisher.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xsub::XSubSocket;
    /// # async fn example(mut xsub: XSubSocket) -> std::io::Result<()> {
    /// // Subscribe to all messages starting with "topic."
    /// xsub.subscribe("topic.").await?;
    ///
    /// // Subscribe to all messages (empty prefix)
    /// xsub.subscribe("").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn subscribe(&mut self, prefix: impl Into<Bytes>) -> io::Result<()> {
        let prefix = prefix.into();
        trace!("[XSUB] Subscribing to: {:?}", prefix);

        self.subscriptions.subscribe(prefix.clone());

        // Send subscription message upstream
        let event = SubscriptionEvent::Subscribe(prefix);
        self.send_subscription_event(event).await?;

        Ok(())
    }

    /// Unsubscribe from messages with the given prefix.
    ///
    /// Optionally sends an unsubscribe message upstream (if verbose mode enabled).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xsub::XSubSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut xsub: XSubSocket) -> std::io::Result<()> {
    /// let prefix = Bytes::from_static(b"topic.");
    /// xsub.unsubscribe(prefix).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn unsubscribe(&mut self, prefix: impl Into<Bytes>) -> io::Result<()> {
        let prefix = prefix.into();
        trace!("[XSUB] Unsubscribing from: {:?}", prefix);

        self.subscriptions.unsubscribe(&prefix);

        // Send unsubscribe message if verbose mode enabled
        if self.base.options.xsub_verbose_unsubs {
            let event = SubscriptionEvent::Unsubscribe(prefix);
            self.send_subscription_event(event).await?;
        }

        Ok(())
    }

    /// Send a raw subscription event upstream (for proxies).
    ///
    /// This allows forwarding subscription messages in broker patterns.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xsub::XSubSocket;
    /// # use monocoque_core::subscription::SubscriptionEvent;
    /// # use bytes::Bytes;
    /// # async fn example(mut xsub: XSubSocket) -> std::io::Result<()> {
    /// let event = SubscriptionEvent::Subscribe(Bytes::from("topic"));
    /// xsub.send_subscription_event(event).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_subscription_event(&mut self, event: SubscriptionEvent) -> io::Result<()> {
        use bytes::BytesMut;
        use compio::buf::BufResult;
        use compio::io::AsyncWriteExt;

        let raw = event.to_message();
        trace!(
            "[XSUB] Sending subscription event ({} bytes): {:?}",
            raw.len(),
            raw
        );

        // Encrypt if CURVE is active; otherwise plain ZMTP frame.
        let mut wire = BytesMut::new();
        if let Some(ref mut cipher) = self.base.curve_cipher {
            let body = cipher
                .encrypt_frame(&raw, false)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            crate::base::append_zmtp_cmd_frame(&mut wire, &body);
        } else {
            crate::codec::encode_multipart(&[raw], &mut wire);
        }
        let wire = wire.freeze();

        let stream =
            self.base.stream.as_mut().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
            })?;

        let data = wire.to_vec();
        let BufResult(result, _) = stream.write_all(data).await;
        result?;

        trace!("[XSUB] Subscription event sent successfully");
        Ok(())
    }

    /// Receive a data message (non-blocking).
    ///
    /// Returns `None` if no message is available.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xsub::XSubSocket;
    /// # async fn example(mut xsub: XSubSocket) -> std::io::Result<()> {
    /// if let Some(msg) = xsub.recv().await? {
    ///     for frame in msg {
    ///         println!("Frame: {:?}", frame);
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        let mut frames: SmallVec<[Bytes; 4]> = SmallVec::new();

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
                        frames.push(payload);
                        if !more {
                            trace!("[XSUB] Received {} frames", frames.len());
                            return Ok(Some(frames.into_vec()));
                        }
                    }
                }
            }

            let n = self.base.read_raw().await?;
            if n == 0 {
                trace!("[XSUB] Connection closed");
                return Ok(None);
            }
            if self.base.check_heartbeat()? {
                self.base.flush_send_buffer().await?;
            }
        }
    }

    /// Get the number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// Check if subscribed to a specific topic.
    pub fn is_subscribed(&self, topic: &[u8]) -> bool {
        self.subscriptions.matches(topic)
    }

    /// Get all subscriptions.
    pub fn subscriptions(&self) -> Vec<monocoque_core::subscription::Subscription> {
        self.subscriptions.subscriptions()
    }

    /// Get the socket type.
    pub const fn socket_type(&self) -> SocketType {
        SocketType::Xsub
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

impl XSubSocket<TcpStream> {
    /// Connect to a publisher, storing the endpoint for automatic reconnection.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xsub::XSubSocket;
    /// # async fn example() -> std::io::Result<()> {
    /// let xsub = XSubSocket::connect("127.0.0.1:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(addr: &str) -> io::Result<Self> {
        Self::connect_with_options(addr, SocketOptions::default()).await
    }

    /// Connect with custom socket options, storing the endpoint for automatic reconnection.
    pub async fn connect_with_options(addr: &str, options: SocketOptions) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        let peer_addr = stream.peer_addr()?;

        let mut stream = stream;
        let handshake_result = crate::handshake::perform_handshake_with_options(
            &mut stream,
            crate::session::SocketType::Xsub,
            None,
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_identity = ?handshake_result.peer_identity,
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[XSUB] Connected to {} (endpoint stored for reconnection)",
            peer_addr
        );

        let endpoint = monocoque_core::endpoint::Endpoint::Tcp(peer_addr);
        let mut base = crate::base::SocketBase::with_endpoint(
            stream,
            crate::session::SocketType::Xsub,
            endpoint,
            options,
        );
        base.curve_cipher = handshake_result.curve_cipher;
        Ok(Self {
            base,
            subscriptions: SubscriptionTrie::new(),
        })
    }

    /// Check if the socket is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.base.is_connected()
    }

    /// Try to reconnect to the stored endpoint, re-sending all active subscriptions.
    pub async fn try_reconnect(&mut self) -> io::Result<()> {
        self.base
            .try_reconnect(crate::session::SocketType::Xsub)
            .await?;
        // Re-send all subscriptions to the fresh connection
        let prefixes: Vec<bytes::Bytes> = self
            .subscriptions
            .subscriptions()
            .iter()
            .map(|s| s.prefix.clone())
            .collect();
        for prefix in prefixes {
            self.send_subscription_event(
                monocoque_core::subscription::SubscriptionEvent::Subscribe(prefix),
            )
            .await?;
        }
        Ok(())
    }

    /// Receive a message with automatic reconnection on EOF or network error.
    ///
    /// Respects `max_reconnect_attempts` — returns `NotConnected` when exhausted.
    pub async fn recv_with_reconnect(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
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
                    "[XSUB] Stream disconnected, reconnecting (attempt {})",
                    attempts
                );
                self.try_reconnect().await?;
            }

            match self.recv().await {
                Ok(Some(msg)) => return Ok(Some(msg)),
                Ok(None) => {
                    debug!("[XSUB] EOF on recv, will reconnect");
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
                        debug!("[XSUB] Connection error on recv ({}), will reconnect", e);
                        self.base.stream = None;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_tracking() {
        use compio::runtime::Runtime;

        Runtime::new().unwrap().block_on(async {
            // Mock stream for testing
            // In real tests, use actual TCP connection
        });
    }

    #[test]
    fn test_subscription_event_creation() {
        let event = SubscriptionEvent::Subscribe(Bytes::from_static(b"topic"));
        let msg = event.to_message();
        assert_eq!(msg[0], 0x01);
        assert_eq!(&msg[1..], b"topic");
    }
}

crate::impl_socket_trait!(XSubSocket<S>, SocketType::Xsub);
