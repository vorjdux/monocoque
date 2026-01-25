//! XPUB (Extended Publisher) socket implementation
//!
//! XPUB extends PUB by receiving subscription messages from subscribers,
//! enabling manual subscription control, last value cache patterns, and
//! subscription forwarding in message brokers.
//!
//! # Use Cases
//!
//! - **Message brokers**: Forward subscriptions between frontend and backend
//! - **Last value cache (LVC)**: Track subscriptions and replay latest values
//! - **Subscription auditing**: Monitor what topics subscribers are interested in
//! - **Manual control**: Explicitly approve/deny subscriptions
//!
//! # Pattern
//!
//! ```text
//! Subscriber 1 ──subscribe("topic.a")──> ┐
//! Subscriber 2 ──subscribe("topic.b")──> ├─> XPUB (receives subscription events)
//! Subscriber 3 ──unsubscribe("topic.a")─> ┘        │
//!                                                   │
//!                                      XPUB ────────┴──> Forwards subscriptions
//! ```

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_core::options::SocketOptions;
use monocoque_core::subscription::{SubscriptionEvent, SubscriptionTrie};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::fmt;
use std::io;
use tracing::{debug, trace};

use crate::handshake::perform_handshake_with_timeout;
use crate::session::SocketType;

/// Unique identifier for each subscriber connection
type SubscriberId = u64;

/// Per-subscriber state managed by XPUB
struct XPubSubscriber {
    id: SubscriberId,
    stream: TcpStream,
    subscriptions: SubscriptionTrie,
}

impl XPubSubscriber {
    /// Check if message matches subscriber's subscriptions
    fn matches(&self, msg: &[Bytes]) -> bool {
        // Check first frame against subscription prefixes
        if let Some(first_frame) = msg.first() {
            self.subscriptions.matches(first_frame)
        } else {
            false
        }
    }
}

/// XPUB (Extended Publisher) socket.
///
/// Receives subscription events and broadcasts messages to matching subscribers.
///
/// # Features
///
/// - **Subscription tracking**: Know what topics subscribers want
/// - **Verbose mode**: Report all subscriptions (including duplicates)
/// - **Manual mode**: Explicit subscription control
/// - **Welcome messages**: Send initial message to new subscribers
///
/// # Examples
///
/// ```no_run
/// use monocoque_zmtp::xpub::XPubSocket;
/// use bytes::Bytes;
///
/// #[compio::main]
/// async fn main() -> std::io::Result<()> {
///     let mut xpub = XPubSocket::bind("127.0.0.1:5555").await?;
///     
///     loop {
///         // Receive subscription events from subscribers
///         if let Some(event) = xpub.recv_subscription().await? {
///             println!("Subscription event: {:?}", event);
///         }
///         
///         // Broadcast messages to matching subscribers
///         xpub.send(vec![Bytes::from("topic"), Bytes::from("data")]).await?;
///     }
/// }
/// ```
pub struct XPubSocket {
    listener: TcpListener,
    subscribers: HashMap<SubscriberId,XPubSubscriber>,
    next_id: SubscriberId,
    options: SocketOptions,
    /// Pending subscription events to deliver
    pending_events: SmallVec<[SubscriptionEvent; 8]>,
}

impl XPubSocket {
    /// Bind to an address and start listening for subscribers.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xpub::XPubSocket;
    /// # async fn example() -> std::io::Result<()> {
    /// let xpub = XPubSocket::bind("127.0.0.1:5555").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn bind(addr: &str) -> io::Result<Self> {
        Self::bind_with_options(addr, SocketOptions::default()).await
    }

    /// Bind with custom socket options.
    pub async fn bind_with_options(
        addr: &str,
        options: SocketOptions,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        debug!("[XPUB] Bound to {}", local_addr);

        Ok(Self {
            listener,
            subscribers: HashMap::new(),
            next_id: 1,
            options,
            pending_events: SmallVec::new(),
        })
    }

    /// Accept new subscriber connections (non-blocking).
    ///
    /// Call this periodically to accept new subscribers.
    pub async fn accept(&mut self) -> io::Result<()> {
        match self.listener.accept().await {
            Ok((mut stream, addr)) => {
                debug!("[XPUB] New subscriber from {}", addr);

                // Perform ZMTP handshake
                let handshake_result = perform_handshake_with_timeout(
                    &mut stream,
                    SocketType::Xpub,
                    None,
                    Some(self.options.handshake_timeout),
                )
                .await?;

                debug!(
                    peer_socket_type = ?handshake_result.peer_socket_type,
                    "[XPUB] Handshake complete with subscriber"
                );

                // Add subscriber
                let id = self.next_id;
                self.next_id += 1;

                // Send welcome message if configured
                if let Some(ref _welcome_msg) = self.options.xpub_welcome_msg {
                    // TODO: Send welcome message
                    trace!("[XPUB] Would send welcome message to subscriber {}", id);
                }

                self.subscribers.insert(
                    id,
                    XPubSubscriber {
                        id,
                        stream,
                        subscriptions: SubscriptionTrie::new(),
                    },
                );

                debug!("[XPUB] Subscriber {} added (total: {})", id, self.subscribers.len());
                Ok(())
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No pending connections
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Receive a subscription event from subscribers (non-blocking).
    ///
    /// Returns `None` if no events are available.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xpub::XPubSocket;
    /// # async fn example(mut xpub: XPubSocket) -> std::io::Result<()> {
    /// if let Some(event) = xpub.recv_subscription().await? {
    ///     match event {
    ///         monocoque_core::subscription::SubscriptionEvent::Subscribe(topic) => {
    ///             println!("New subscription: {:?}", topic);
    ///         }
    ///         monocoque_core::subscription::SubscriptionEvent::Unsubscribe(topic) => {
    ///             println!("Unsubscription: {:?}", topic);
    ///         }
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv_subscription(&mut self) -> io::Result<Option<SubscriptionEvent>> {
        use compio::buf::BufResult;
        use compio::io::AsyncRead;
        use compio::time::timeout;
        use std::time::Duration;
        
        // Return pending events first
        if !self.pending_events.is_empty() {
            return Ok(Some(self.pending_events.remove(0)));
        }

        // NOTE: Don't call accept() here - it blocks waiting for new connections
        // The caller should call accept() separately to handle new connections

        // Poll all subscribers for subscription messages
        // Subscription messages are 1 byte (0x00 or 0x01) + topic prefix
        println!("[XPUB recv_subscription] Polling {} subscribers", self.subscribers.len());
        for sub in self.subscribers.values_mut() {
            // Try non-blocking read of subscription message with timeout
            let buf = vec![0u8; 256];
            println!("[XPUB recv_subscription] Reading from subscriber {} with timeout", sub.id);
            
            // Use a short timeout to avoid blocking
            let read_result = timeout(Duration::from_millis(1), sub.stream.read(buf)).await;
            
            match read_result {
                Ok(BufResult(Ok(n), buf)) if n > 0 => {
                    println!("[XPUB recv_subscription] Received {} bytes: {:?}", n, &buf[..n]);
                    // Parse subscription event
                    if let Some(event) = SubscriptionEvent::from_message(&buf[..n]) {
                        trace!("[XPUB] Received subscription event: {:?}", event);
                        println!("[XPUB recv_subscription] Parsed event: {:?}", event);
                        
                        // Update subscriber's subscriptions
                        match &event {
                            SubscriptionEvent::Subscribe(prefix) => {
                                sub.subscriptions.subscribe(prefix.clone());
                            }
                            SubscriptionEvent::Unsubscribe(prefix) => {
                                sub.subscriptions.unsubscribe(&prefix);
                            }
                        }
                        
                        // Return event if in verbose mode
                        if self.options.xpub_verbose {
                            println!("[XPUB recv_subscription] Returning event (verbose=true)");
                            return Ok(Some(event));
                        } else {
                            println!("[XPUB recv_subscription] NOT returning event (verbose=false)");
                        }
                    } else {
                        println!("[XPUB recv_subscription] Failed to parse subscription event");
                    }
                }
                Ok(BufResult(Ok(_), _)) => {
                    println!("[XPUB recv_subscription] No data available (n=0)");
                }
                Ok(BufResult(Err(e), _)) => {
                    println!("[XPUB recv_subscription] Read error: {}", e);
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        debug!("[XPUB] Error reading from subscriber: {}", e);
                    }
                }
                Err(_) => {
                    // Timeout - no data available
                    println!("[XPUB recv_subscription] Read timeout (no data)");
                    continue;
                }
            }
        }
        
        println!("[XPUB recv_subscription] No subscription event found");
        Ok(None)
    }

    /// Broadcast a message to all matching subscribers.
    ///
    /// Only subscribers whose subscriptions match the message's first frame
    /// will receive it.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xpub::XPubSocket;
    /// # use bytes::Bytes;
    /// # async fn example(mut xpub: XPubSocket) -> std::io::Result<()> {
    /// xpub.send(vec![
    ///     Bytes::from("topic.temperature"),
    ///     Bytes::from("23.5"),
    /// ]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        trace!("[XPUB] Broadcasting message with {} frames", msg.len());

        let dead_subs = Vec::new();

        for sub in self.subscribers.values_mut() {
            // Skip non-matching subscribers
            if !sub.matches(&msg) {
                continue;
            }

            // TODO: Actually send the message to the subscriber
            // For now, just track that we would send it
            trace!("[XPUB] Would send to subscriber {}", sub.id);
        }

        // Remove dead subscribers
        for id in dead_subs {
            self.subscribers.remove(&id);
            debug!("[XPUB] Removed dead subscriber {}", id);
        }

        Ok(())
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Get the local address.
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Get the socket type.
    pub fn socket_type(&self) -> SocketType {
        SocketType::Xpub
    }

    /// Check if the last received message has more frames coming.
    ///
    /// For XPUB, subscription events are always single-frame.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) option.
    #[inline]
    pub fn has_more(&self) -> bool {
        !self.pending_events.is_empty()
    }

    /// Get the event state of the socket.
    ///
    /// Returns a bitmask indicating ready-to-receive and ready-to-send states.
    ///
    /// # Returns
    ///
    /// - `1` (POLLIN) - Socket is ready to receive (has pending subscription events)
    /// - `2` (POLLOUT) - Socket is ready to send (has active subscribers)
    /// - `3` (POLLIN | POLLOUT) - Socket is ready for both
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_EVENTS` (15) option.
    #[inline]
    pub fn events(&self) -> u32 {
        let mut events = 0;
        if !self.pending_events.is_empty() {
            events |= 1; // POLLIN
        }
        if !self.subscribers.is_empty() {
            events |= 2; // POLLOUT
        }
        events
    }

    /// Set verbose mode.
    ///
    /// When enabled, all subscription messages are reported (including duplicates).
    pub fn set_verbose(&mut self, verbose: bool) {
        self.options.xpub_verbose = verbose;
    }

    /// Set manual mode.
    ///
    /// When enabled, subscriptions must be explicitly approved by calling `send_subscription()`.
    pub fn set_manual(&mut self, manual: bool) {
        self.options.xpub_manual = manual;
    }

    /// Manually send a subscription message (manual mode only).
    ///
    /// This allows explicit control over subscription forwarding.
    pub async fn send_subscription(&mut self, event: SubscriptionEvent) -> io::Result<()> {
        if !self.options.xpub_manual {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Manual mode not enabled",
            ));
        }

        // TODO: Forward subscription to upstream
        trace!("[XPUB] Manual subscription: {:?}", event);
        Ok(())
    }
}

impl fmt::Debug for XPubSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XPubSocket")
            .field("subscribers", &self.subscribers.len())
            .field("verbose", &self.options.xpub_verbose)
            .field("manual", &self.options.xpub_manual)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[compio::test]
    async fn test_xpub_bind() {
        let xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(xpub.subscriber_count(), 0);
        let addr = xpub.local_addr().unwrap();
        assert!(addr.port() > 0);
    }

    #[test]
    fn test_subscription_event_encoding() {
        let event = SubscriptionEvent::Subscribe(Bytes::from_static(b"topic"));
        let msg = event.to_message();
        assert_eq!(msg[0], 0x01);
        assert_eq!(&msg[1..], b"topic");

        let parsed = SubscriptionEvent::from_message(&msg).unwrap();
        assert_eq!(parsed, event);
    }
}

// Implement Socket trait for XPubSocket (non-generic)
#[async_trait::async_trait(?Send)]
impl crate::Socket for XPubSocket {
    async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        // XPUB receives subscription events
        self.recv_subscription().await.map(|opt| {
            opt.map(|event| {
                vec![event.to_message()]
            })
        })
    }

    fn socket_type(&self) -> SocketType {
        SocketType::Xpub
    }
}
