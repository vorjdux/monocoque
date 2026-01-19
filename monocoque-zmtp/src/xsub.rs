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
use monocoque_core::config::BufferConfig;
use monocoque_core::options::SocketOptions;
use monocoque_core::subscription::{SubscriptionEvent, SubscriptionTrie};
use smallvec::SmallVec;
use std::io;
use tracing::{debug, trace};

use crate::handshake::perform_handshake_with_timeout;
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
///     xsub.subscribe(b"topic.").await?;
///     
///     // Receive messages
///     if let Some(msg) = xsub.recv().await? {
///         println!("Received: {:?}", msg);
///     }
///     
///     // Unsubscribe
///     xsub.unsubscribe(b"topic.").await?;
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
    /// Accumulated frames for current multipart message
    frames: SmallVec<[Bytes; 4]>,
    /// Local subscription tracking
    subscriptions: SubscriptionTrie,
}

impl<S> XSubSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new XSUB socket from a stream.
    pub async fn new(stream: S) -> io::Result<Self> {
        Self::with_options(stream, BufferConfig::large(), SocketOptions::default()).await
    }

    /// Create a new XSUB socket with custom configuration.
    pub async fn with_config(stream: S, config: BufferConfig) -> io::Result<Self> {
        Self::with_options(stream, config, SocketOptions::default()).await
    }

    /// Create a new XSUB socket with custom configuration and options.
    pub async fn with_options(
        mut stream: S,
        config: BufferConfig,
        options: SocketOptions,
    ) -> io::Result<Self> {
        debug!("[XSUB] Creating new XSUB socket");

        // Perform ZMTP handshake
        debug!("[XSUB] Performing ZMTP handshake...");
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Xsub,
            None,
            Some(options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(
            peer_socket_type = ?handshake_result.peer_socket_type,
            "[XSUB] Handshake complete"
        );

        Ok(Self {
            base: SocketBase::new(stream, config, options),
            frames: SmallVec::new(),
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
    /// xsub.subscribe(b"topic.").await?;
    /// 
    /// // Subscribe to all messages (empty prefix)
    /// xsub.subscribe(b"").await?;
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
    /// xsub.unsubscribe(&prefix).await?;
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
        use compio::buf::BufResult;
        use compio::io::AsyncWrite;
        use monocoque_core::alloc::IoBytes;

        let msg = event.to_message();
        trace!("[XSUB] Sending subscription event ({} bytes)", msg.len());
        
        let stream = self.base.stream.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Socket not connected")
        })?;
        
        let BufResult(result, _) = AsyncWrite::write(stream, IoBytes::new(msg)).await;
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
        // TODO: Implement actual message reception
        // For now, return None
        Ok(None)
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
    pub fn subscriptions(&self) -> &[monocoque_core::subscription::Subscription] {
        self.subscriptions.subscriptions()
    }

    /// Get the socket type.
    pub fn socket_type(&self) -> SocketType {
        SocketType::Xsub
    }
}

impl XSubSocket<TcpStream> {
    /// Connect to a publisher.
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
        let stream = TcpStream::connect(addr).await?;
        Self::new(stream).await
    }

    /// Connect with custom socket options.
    pub async fn connect_with_options(
        addr: &str,
        options: SocketOptions,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Self::with_options(stream, BufferConfig::large(), options).await
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
