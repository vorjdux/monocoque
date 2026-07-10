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

use bytes::{Bytes, BytesMut};
use monocoque_core::io::take_read_buffer;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{TcpListener, TcpStream};
use monocoque_core::subscription::{SubscriptionEvent, SubscriptionTrie};
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use tracing::{debug, trace};

use crate::handshake::perform_handshake_with_options;
use crate::session::SocketType;
use crate::xsub::XSubSocket;

/// Unique identifier for each subscriber connection
type SubscriberId = u64;

/// Per-subscriber state managed by XPUB
struct XPubSubscriber {
    id: SubscriberId,
    stream: TcpStream,
    subscriptions: SubscriptionTrie,
    recv_buf: monocoque_core::buffer::SegmentedBuffer,
    read_buf: BytesMut,
    decoder: crate::codec::ZmtpDecoder,
    curve_cipher: Option<crate::security::curve::CurveMessageCipher>,
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
/// # async fn example() -> std::io::Result<()> {
/// let mut xpub = XPubSocket::bind("127.0.0.1:5555").await?;
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
/// # }
/// ```
pub struct XPubSocket {
    listener: TcpListener,
    subscribers: HashMap<SubscriberId, XPubSubscriber>,
    next_id: SubscriberId,
    options: SocketOptions,
    /// Pending subscription events to deliver
    pending_events: SmallVec<[SubscriptionEvent; 8]>,
    /// Optional upstream connection for manual-mode subscription forwarding.
    ///
    /// When set, `send_subscription()` writes subscription events to this
    /// connection so they propagate to the upstream publisher.
    upstream: Option<XSubSocket<TcpStream>>,
    /// Tracks which unique topic prefixes currently have at least one subscriber.
    ///
    /// Used in non-verbose mode to deliver an event only the FIRST time a topic
    /// is subscribed (and when it transitions back to zero subscribers).
    seen_topics: HashSet<Vec<u8>>,
    /// Reference-count of active subscriptions per topic prefix.
    ///
    /// Maps topic prefix → number of active subscribers interested in it.
    /// When the count drops to zero, the topic is removed from `seen_topics`
    /// and an Unsubscribe event is delivered.
    topic_refcount: HashMap<Vec<u8>, usize>,
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
    pub async fn bind_with_options(addr: &str, options: SocketOptions) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        debug!("[XPUB] Bound to {}", local_addr);

        Ok(Self {
            listener,
            subscribers: HashMap::new(),
            next_id: 1,
            options,
            pending_events: SmallVec::new(),
            upstream: None,
            seen_topics: HashSet::new(),
            topic_refcount: HashMap::new(),
        })
    }

    /// Accept new subscriber connections (non-blocking).
    ///
    /// Call this periodically to accept new subscribers.
    pub async fn accept(&mut self) -> io::Result<()> {
        match self.listener.accept().await {
            Ok((mut stream, addr)) => {
                debug!("[XPUB] New subscriber from {}", addr);

                // Enable TCP_NODELAY (and keepalive) on the accepted subscriber,
                // matching PUB's accept path. One-time setsockopt at accept, not
                // in the publish hot path.
                crate::utils::configure_tcp_stream(&stream, &self.options, "XPUB")?;

                // Perform ZMTP handshake
                let handshake_result = perform_handshake_with_options(
                    &mut stream,
                    SocketType::Xpub,
                    None,
                    Some(self.options.handshake_timeout),
                    &self.options,
                )
                .await?;

                debug!(
                    peer_socket_type = ?handshake_result.peer_socket_type,
                    "[XPUB] Handshake complete with subscriber"
                );

                // Add subscriber
                let id = self.next_id;
                self.next_id += 1;

                let mut curve_cipher = handshake_result.curve_cipher;

                // Send welcome message if configured
                if let Some(ref welcome_msg) = self.options.xpub_welcome_msg.clone() {
                    use bytes::BytesMut;
                    use compio_buf::BufResult;
                    use compio_io::AsyncWriteExt;

                    let wire = if let Some(ref mut cipher) = curve_cipher {
                        let mut buf = BytesMut::new();
                        let body = cipher.encrypt_frame(welcome_msg, false).map_err(|e| {
                            io::Error::new(io::ErrorKind::InvalidData, e.to_string())
                        })?;
                        crate::base::append_zmtp_cmd_frame(&mut buf, &body);
                        buf.freeze()
                    } else {
                        let mut buf = BytesMut::with_capacity(welcome_msg.len() + 9);
                        crate::codec::encode_multipart(std::slice::from_ref(welcome_msg), &mut buf);
                        buf.freeze()
                    };

                    let BufResult(result, _) = stream.write_all(wire).await;
                    if let Err(e) = result {
                        trace!(
                            "[XPUB] Failed to send welcome message to subscriber {}: {}",
                            id, e
                        );
                    }
                }

                self.subscribers.insert(
                    id,
                    XPubSubscriber {
                        id,
                        stream,
                        subscriptions: SubscriptionTrie::new(),
                        recv_buf: monocoque_core::buffer::SegmentedBuffer::new(),
                        read_buf: BytesMut::new(),
                        decoder: self.options.max_msg_size.map_or_else(
                            crate::codec::ZmtpDecoder::new,
                            crate::codec::ZmtpDecoder::with_max_frame_size,
                        ),
                        curve_cipher,
                    },
                );

                debug!(
                    "[XPUB] Subscriber {} added (total: {})",
                    id,
                    self.subscribers.len()
                );
                Ok(())
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No pending connections
                Ok(())
            }
            Err(e) => {
                // Throttle on fd exhaustion so a caller's accept loop cannot
                // livelock while no descriptors are available.
                crate::utils::backoff_on_fd_exhaustion(&e).await;
                Err(e)
            }
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
    #[allow(clippy::too_many_lines)]
    pub async fn recv_subscription(&mut self) -> io::Result<Option<SubscriptionEvent>> {
        use compio_buf::BufResult;
        use compio_io::AsyncRead;
        use monocoque_core::rt::timeout;
        use std::time::Duration;

        // Return pending events first
        if !self.pending_events.is_empty() {
            return Ok(Some(self.pending_events.remove(0)));
        }

        // NOTE: Don't call accept() here - it blocks waiting for new connections
        // The caller should call accept() separately to handle new connections

        // Poll all subscribers for subscription messages
        trace!(
            "[XPUB] Polling {} subscribers for subscription events",
            self.subscribers.len()
        );
        for sub in self.subscribers.values_mut() {
            // SAFETY: `slab` is passed straight to `read`; the data arm below
            // truncates it to `n` before freezing, and every other arm drops it
            // without inspecting its contents.
            let slab = unsafe { take_read_buffer(&mut sub.read_buf, 256) };

            // Use a short timeout to avoid blocking
            let read_result = timeout(Duration::from_millis(1), sub.stream.read(slab)).await;

            match read_result {
                Ok(BufResult(Ok(n), mut slab)) if n > 0 => {
                    trace!("[XPUB] Received {} bytes from subscriber {}", n, sub.id);
                    debug_assert!(n <= 256);
                    slab.truncate(n);
                    sub.recv_buf.push(slab.freeze());

                    // Drain all complete ZMTP frames from the buffer
                    loop {
                        match sub.decoder.decode(&mut sub.recv_buf) {
                            Ok(Some(frame)) => {
                                // Resolve the subscription payload, handling CURVE decryption.
                                let payload = if frame.is_command() {
                                    if let Some(ref mut cipher) = sub.curve_cipher {
                                        if crate::security::curve::CurveMessageCipher::is_curve_message(&frame.payload) {
                                            match cipher.decrypt_frame(&frame.payload) {
                                                Ok((_more, data)) => data,
                                                Err(_) => continue,
                                            }
                                        } else {
                                            // Non-MESSAGE command (e.g. PING): handle and skip.
                                            if crate::base::is_ping_payload(&frame.payload) {
                                                use compio_io::AsyncWriteExt;
                                                let pong = crate::base::build_pong_frame();
                                                let BufResult(result, _) = sub.stream.write_all(pong).await;
                                                let _ = result;
                                            }
                                            continue;
                                        }
                                    } else {
                                        if crate::base::is_ping_payload(&frame.payload) {
                                            use compio_io::AsyncWriteExt;
                                            let pong = crate::base::build_pong_frame();
                                            let BufResult(result, _) =
                                                sub.stream.write_all(pong).await;
                                            let _ = result;
                                        }
                                        continue;
                                    }
                                } else {
                                    frame.payload
                                };
                                if let Some(event) = SubscriptionEvent::from_bytes(payload) {
                                    trace!(
                                        "[XPUB] Subscription event from subscriber {}: {:?}",
                                        sub.id, event
                                    );

                                    let should_deliver = if self.options.xpub_verbose {
                                        // Verbose mode: always deliver every event
                                        match &event {
                                            SubscriptionEvent::Subscribe(prefix) => {
                                                sub.subscriptions.subscribe(prefix.clone());
                                                let key = prefix.to_vec();
                                                *self
                                                    .topic_refcount
                                                    .entry(key.clone())
                                                    .or_insert(0) += 1;
                                                self.seen_topics.insert(key);
                                            }
                                            SubscriptionEvent::Unsubscribe(prefix) => {
                                                sub.subscriptions.unsubscribe(prefix);
                                                let key = prefix.to_vec();
                                                let count = self
                                                    .topic_refcount
                                                    .entry(key.clone())
                                                    .or_insert(0);
                                                if *count > 0 {
                                                    *count -= 1;
                                                }
                                                if *count == 0 {
                                                    self.seen_topics.remove(&key);
                                                    self.topic_refcount.remove(&key);
                                                }
                                            }
                                        }
                                        true
                                    } else {
                                        // Non-verbose mode: deliver only on first subscribe / last unsubscribe
                                        match &event {
                                            SubscriptionEvent::Subscribe(prefix) => {
                                                sub.subscriptions.subscribe(prefix.clone());
                                                let key = prefix.to_vec();
                                                let count = self
                                                    .topic_refcount
                                                    .entry(key.clone())
                                                    .or_insert(0);
                                                *count += 1;
                                                if *count == 1 {
                                                    // First subscriber for this topic
                                                    self.seen_topics.insert(key);
                                                    true
                                                } else {
                                                    false
                                                }
                                            }
                                            SubscriptionEvent::Unsubscribe(prefix) => {
                                                sub.subscriptions.unsubscribe(prefix);
                                                let key = prefix.to_vec();
                                                let count = self
                                                    .topic_refcount
                                                    .entry(key.clone())
                                                    .or_insert(0);
                                                if *count > 0 {
                                                    *count -= 1;
                                                }
                                                if *count == 0 {
                                                    // Last subscriber gone for this topic
                                                    self.seen_topics.remove(&key);
                                                    self.topic_refcount.remove(&key);
                                                    true
                                                } else {
                                                    false
                                                }
                                            }
                                        }
                                    };

                                    if should_deliver {
                                        self.pending_events.push(event);
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }
                }
                Ok(BufResult(Ok(_), _)) => {}
                Ok(BufResult(Err(e), _)) => {
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        debug!("[XPUB] Error reading from subscriber {}: {}", sub.id, e);
                    }
                }
                Err(_) => {
                    // Timeout  -  no data available from this subscriber
                }
            }
        }

        // Return any events collected from this poll round
        if !self.pending_events.is_empty() {
            return Ok(Some(self.pending_events.remove(0)));
        }

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
        use bytes::BytesMut;
        use compio_buf::BufResult;
        use compio_io::AsyncWriteExt;

        trace!("[XPUB] Broadcasting message with {} frames", msg.len());

        // Pre-encode once for plaintext subscribers (shared via O(1) clone).
        // Encrypted subscribers get per-subscriber encoding below.
        let mut plain_wire: Option<bytes::Bytes> = None;

        let mut dead_subs = Vec::new();

        for sub in self.subscribers.values_mut() {
            if !sub.matches(&msg) {
                continue;
            }

            let wire = if let Some(ref mut cipher) = sub.curve_cipher {
                let last = msg.len().saturating_sub(1);
                let mut buf = BytesMut::new();
                let mut ok = true;
                for (i, frame) in msg.iter().enumerate() {
                    if let Ok(body) = cipher.encrypt_frame(frame, i < last) {
                        crate::base::append_zmtp_cmd_frame(&mut buf, &body);
                    } else {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    dead_subs.push(sub.id);
                    continue;
                }
                buf.freeze()
            } else {
                plain_wire
                    .get_or_insert_with(|| {
                        let wire_capacity: usize = msg
                            .iter()
                            .map(|part| part.len() + if part.len() >= 256 { 9 } else { 2 })
                            .sum();
                        let mut buf = BytesMut::with_capacity(wire_capacity);
                        crate::codec::encode_multipart(&msg, &mut buf);
                        buf.freeze()
                    })
                    .clone()
            };

            let BufResult(result, _) = sub.stream.write_all(wire).await;
            if let Err(e) = result {
                debug!("[XPUB] Failed to send to subscriber {}: {}", sub.id, e);
                dead_subs.push(sub.id);
            } else {
                trace!("[XPUB] Sent to subscriber {}", sub.id);
            }
        }

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
    pub const fn socket_type(&self) -> SocketType {
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

    /// Connect to an upstream publisher so that subscription events can be forwarded.
    ///
    /// The upstream is typically a PUB or XSUB socket.  After calling this method,
    /// `send_subscription()` (manual mode) writes subscription messages to the upstream
    /// connection, causing the upstream publisher to start or stop delivering matching
    /// messages.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use monocoque_zmtp::xpub::XPubSocket;
    /// # use monocoque_core::subscription::SubscriptionEvent;
    /// # use bytes::Bytes;
    /// # async fn example() -> std::io::Result<()> {
    /// let mut xpub = XPubSocket::bind("127.0.0.1:5556").await?;
    /// xpub.set_manual(true);
    /// xpub.connect_upstream("127.0.0.1:5555").await?;
    ///
    /// // Receive a subscription from a downstream client and forward it upstream.
    /// if let Some(event) = xpub.recv_subscription().await? {
    ///     xpub.send_subscription(event).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_upstream(&mut self, addr: &str) -> io::Result<()> {
        debug!("[XPUB] Connecting upstream to {}", addr);
        let xsub = XSubSocket::connect(addr).await?;
        self.upstream = Some(xsub);
        debug!("[XPUB] Upstream connected");
        Ok(())
    }

    /// Manually send a subscription event to the upstream connection.
    ///
    /// Requires both manual mode (`set_manual(true)`) and an upstream connection
    /// (`connect_upstream()`).  Writes the subscription message directly to the
    /// upstream publisher so it starts (or stops) delivering matching messages.
    pub async fn send_subscription(&mut self, event: SubscriptionEvent) -> io::Result<()> {
        if !self.options.xpub_manual {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Manual mode not enabled",
            ));
        }

        let upstream = self.upstream.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "No upstream connection; call connect_upstream() first",
            )
        })?;

        trace!("[XPUB] Forwarding subscription upstream: {:?}", event);
        upstream.send_subscription_event(event).await
    }
}

impl fmt::Debug for XPubSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XPubSocket")
            .field("subscribers", &self.subscribers.len())
            .field("verbose", &self.options.xpub_verbose)
            .field("manual", &self.options.xpub_manual)
            .field("has_upstream", &self.upstream.is_some())
            .finish()
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
        self.recv_subscription()
            .await
            .map(|opt| opt.map(|event| vec![event.to_message()]))
    }

    fn socket_type(&self) -> SocketType {
        SocketType::Xpub
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publisher::PubSocket as InternalPub;

    /// XPUB must set TCP_NODELAY on accepted subscriber connections, matching
    /// PUB. Connects a real XSUB peer, then reads TCP_NODELAY off the stored
    /// subscriber's socket fd. One-time setsockopt at accept - off the hot path.
    #[cfg(unix)]
    #[test]
    fn xpub_accept_sets_tcp_nodelay() {
        use monocoque_core::rt::LocalRuntime;
        use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
        use std::sync::mpsc;
        use std::thread;

        fn fd_nodelay(fd: RawFd) -> bool {
            let sock = unsafe { socket2::Socket::from_raw_fd(fd) };
            let nd = sock.nodelay().expect("query TCP_NODELAY");
            std::mem::forget(sock); // borrowed fd - do not close it
            nd
        }

        let (port_tx, port_rx) = mpsc::channel::<u16>();
        let (done_tx, done_rx) = mpsc::channel::<()>();

        // XSUB client: connect once the port is known, then hold the connection.
        let client = thread::spawn(move || {
            let rt = LocalRuntime::new().unwrap();
            rt.block_on(async move {
                let port = port_rx.recv().unwrap();
                let _xsub = crate::xsub::XSubSocket::connect(&format!("127.0.0.1:{port}"))
                    .await
                    .unwrap();
                done_rx.recv().unwrap();
            });
        });

        let rt = LocalRuntime::new().unwrap();
        let nodelay = rt.block_on(async move {
            let mut xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(xpub.local_addr().unwrap().port()).unwrap();
            xpub.accept().await.unwrap();
            let sub = xpub.subscribers.values().next().expect("one subscriber");
            fd_nodelay(sub.stream.as_raw_fd())
        });
        done_tx.send(()).unwrap();
        client.join().unwrap();
        assert!(
            nodelay,
            "XPUB accept must set TCP_NODELAY on subscriber sockets",
        );
    }

    #[test]
    fn test_xpub_bind() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_xpub_bind_impl());
    }

    async fn test_xpub_bind_impl() {
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

    /// `send_subscription` errors when manual mode is off.
    #[test]
    fn test_send_subscription_requires_manual_mode() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_send_subscription_requires_manual_mode_impl());
    }

    async fn test_send_subscription_requires_manual_mode_impl() {
        let mut xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
        // manual mode is off by default
        let err = xpub
            .send_subscription(SubscriptionEvent::Subscribe(Bytes::from("topic")))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// `send_subscription` errors when no upstream is connected.
    #[test]
    fn test_send_subscription_requires_upstream() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_send_subscription_requires_upstream_impl());
    }

    async fn test_send_subscription_requires_upstream_impl() {
        let mut xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
        xpub.set_manual(true);
        let err = xpub
            .send_subscription(SubscriptionEvent::Subscribe(Bytes::from("topic")))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotConnected);
    }

    /// `connect_upstream` + `send_subscription` forward subscription bytes to a PubSocket.
    ///
    /// The PubSocket's subscription reader (running inside a worker thread) picks up the
    /// raw subscription bytes written by the upstream XSubSocket. We verify this
    /// indirectly: after forwarding Subscribe("weather"), publishing a "weather" message
    /// reaches the upstream connection (the XSubSocket), confirming the PUB socket
    /// started delivering matching messages.
    #[test]
    fn test_connect_upstream_and_forward_subscription() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_connect_upstream_and_forward_subscription_impl());
    }

    async fn test_connect_upstream_and_forward_subscription_impl() {
        use monocoque_core::rt::TcpListener;

        // Bind a PubSocket listener (the upstream data source).
        let pub_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pub_addr = pub_listener.local_addr().unwrap();

        // Spawn PubSocket: accept the XSubSocket upstream connection, then broadcast.
        let pub_task = monocoque_core::rt::spawn(async move {
            let mut pub_sock = InternalPub::new();
            // Accept the connection that connect_upstream() will make.
            pub_sock.accept_subscriber(&pub_listener).await.unwrap();
            // Give the subscription reader time to process Subscribe("weather").
            monocoque_core::rt::sleep(std::time::Duration::from_millis(50)).await;
            // Broadcast a matching message  -  should reach the upstream XSubSocket.
            pub_sock
                .send(vec![Bytes::from("weather"), Bytes::from("sunny")])
                .await
                .unwrap();
        });

        let mut xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
        xpub.set_manual(true);

        // Connect upstream to the PubSocket listener.
        xpub.connect_upstream(&pub_addr.to_string()).await.unwrap();
        assert!(xpub.upstream.is_some());

        // Forward a subscription to the PubSocket.
        xpub.send_subscription(SubscriptionEvent::Subscribe(Bytes::from("weather")))
            .await
            .unwrap();

        // Wait for the PubSocket to broadcast.
        monocoque_core::rt::join(pub_task).await;

        // The upstream XSubSocket should have received the "weather" message.
        let msg = xpub
            .upstream
            .as_mut()
            .unwrap()
            .recv()
            .await
            .unwrap()
            .expect("upstream should have received matching message");

        assert_eq!(msg[0], Bytes::from("weather"));
        assert_eq!(msg[1], Bytes::from("sunny"));
    }
}
