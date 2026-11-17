//! Socket configuration options
//!
//! This module provides configuration options for `ZeroMQ` sockets, similar to
//! libzmq's socket options (`zmq_setsockopt/zmq_getsockopt`).

use std::{fmt, time::Duration};

/// Smallest usable read buffer.
///
/// A read buffer of 0 (or a byte or two) turns the read loop into a spin or a
/// false EOF, so every setter floors to this.
const MIN_READ_BUFFER_SIZE: usize = 64;

/// Clamp a caller-supplied read buffer size into the usable range.
const fn clamp_read_buffer_size(size: usize) -> usize {
    if size < MIN_READ_BUFFER_SIZE {
        MIN_READ_BUFFER_SIZE
    } else if size > crate::io::MAX_READ_SIZE {
        crate::io::MAX_READ_SIZE
    } else {
        size
    }
}

/// Socket configuration options.
///
/// These options control socket behavior including timeouts, buffer sizes,
/// and reliability features. This struct consolidates all socket configuration
/// in one place, following the `MongoDB` Rust driver pattern.
///
/// # Examples
///
/// ```
/// use monocoque_core::options::SocketOptions;
/// use std::time::Duration;
///
/// // Simple case: use defaults
/// let opts = SocketOptions::default();
///
/// // Customize timeouts and buffers
/// let opts = SocketOptions::default()
///     .with_recv_timeout(Duration::from_secs(5))
///     .with_send_timeout(Duration::from_secs(5))
///     .with_read_buffer_size(65536); // widen the read batch for bulk transfers
/// ```
///
/// New options are added as fields, so the struct cannot be built with a
/// struct literal outside this crate: start from [`SocketOptions::default`]
/// and use the `with_*` methods.
#[derive(Clone)]
#[non_exhaustive]
pub struct SocketOptions {
    /// Read buffer size (bytes)
    ///
    /// How much room each peer's reader reserves in its read buffer before a
    /// read, and so the most one read can return.
    /// - Default: 32768 (32KB) - cuts recv syscalls on bulk transfers
    /// - Small (4KB): Low-latency with small messages (< 1KB)
    /// - Large (up to 65536): fewer recv syscalls on bulk transfers, at the
    ///   cost of a larger buffer per peer
    ///
    /// Values below 64 are raised to 64 and values above 65536 are lowered to
    /// 65536. Note that anything under the 32KB default trades throughput away,
    /// so reach for a smaller buffer only to cut latency, not to "tune".
    pub read_buffer_size: usize,

    /// Receive timeout (`ZMQ_RCVTIMEO`)
    ///
    /// Maximum time to wait for a receive operation.
    /// - `None`: Wait indefinitely (default)
    /// - `Some(Duration::ZERO)`: Do not wait; fail with `Again` when no
    ///   message is queued
    /// - `Some(duration)`: Wait up to duration, then fail with `TimedOut`
    pub recv_timeout: Option<Duration>,

    /// Send timeout (`ZMQ_SNDTIMEO`)
    ///
    /// Maximum time to wait for a send operation.
    /// - `None`: Wait indefinitely (default)
    /// - `Some(Duration::ZERO)`: Do not wait; fail with `Again` when the
    ///   message cannot be taken at once
    /// - `Some(duration)`: Wait up to duration, then fail with `TimedOut`
    pub send_timeout: Option<Duration>,

    /// Handshake timeout (`ZMQ_HANDSHAKE_IVL`)
    ///
    /// Maximum time to complete ZMTP handshake after connection.
    /// - Default: 30 seconds
    /// - `Duration::ZERO`: no limit, as in libzmq. A peer that connects and
    ///   says nothing then holds its connection open until it closes it.
    pub handshake_timeout: Duration,

    /// Linger timeout (`ZMQ_LINGER`)
    ///
    /// How long `close()` waits for queued messages to be written.
    /// - `Some(duration)`: wait up to duration (default: 30 seconds)
    /// - `Some(Duration::ZERO)`: do not wait; what is still queued is dropped
    /// - `None`: wait until everything is written, however long that takes
    ///   (libzmq's `-1`). `close()` then never returns while a peer does not
    ///   read.
    pub linger: Option<Duration>,

    /// How long an eager send waits on a peer taking under 16 KiB of bytes
    /// before treating it as no longer reading (default 50 ms; noticing takes
    /// up to two windows). Sends to it then stop waiting, so it cannot hold up
    /// the socket's other peers, and the monitor reports `PeerStalled`.
    ///
    /// - `Some(window)`: give up on such a peer after `window`
    /// - `None`: always wait until the bytes are in the kernel, bounded only by
    ///   `send_timeout`; one peer that stops reading then blocks the sender
    pub write_stall: Option<Duration>,

    /// Reconnect interval (`ZMQ_RECONNECT_IVL`)
    ///
    /// Initial reconnection delay after connection loss.
    /// - Default: 100ms
    /// - Use with `reconnect_ivl_max` for exponential backoff
    pub reconnect_ivl: Duration,

    /// Maximum reconnect interval (`ZMQ_RECONNECT_IVL_MAX`)
    ///
    /// Maximum reconnection delay for exponential backoff.
    /// - Default: 0 (no maximum, use `reconnect_ivl` always)
    /// - When > 0: Doubles `reconnect_ivl` up to this value
    pub reconnect_ivl_max: Duration,

    /// Connection timeout (`ZMQ_CONNECT_TIMEOUT`)
    ///
    /// Maximum time one dial may take, from the start of the connect to an
    /// established TCP or Unix stream. A dial that runs out counts as a failed
    /// attempt and is retried after the reconnect interval.
    /// - Default: 0 (no limit of ours; the OS gives up in its own time)
    pub connect_timeout: Duration,

    /// High water mark for receiving (`ZMQ_RCVHWM`)
    ///
    /// Complete messages queued per peer and not yet received. A peer at the
    /// mark is not read from until the application receives, so TCP flow
    /// control (or the inproc queue) pushes back on that sender alone; the
    /// socket's other peers keep flowing. Ignored under [`conflate`].
    /// - Default: 1000 messages
    /// - `0`: unbounded, as in libzmq. A peer that outpaces the application
    ///   then grows memory without limit.
    ///
    /// [`conflate`]: Self::conflate
    pub recv_hwm: usize,

    /// High water mark for sending (`ZMQ_SNDHWM`)
    ///
    /// Messages queued per peer and not yet written. At the mark, a socket
    /// whose pattern waits (PUSH, DEALER, REQ, PAIR) waits for room, and one
    /// whose pattern drops (PUB, XPUB, ROUTER) drops the message for that peer.
    /// - Default: 1000 messages
    /// - `0`: unbounded, as in libzmq.
    pub send_hwm: usize,

    /// Largest frame a peer may send (`ZMQ_MAXMSGSIZE`)
    ///
    /// The limit is on each frame's body, in bytes. A peer that announces a
    /// larger frame is disconnected before the frame is read. A multipart
    /// message may be larger in total, up to 256 MiB.
    /// - `None`: 64 MiB (default)
    /// - `Some(size)`: `size` bytes, lowered to 256 MiB when larger
    pub max_msg_size: Option<usize>,

    /// Socket identity / routing ID (`ZMQ_ROUTING_ID` / `ZMQ_IDENTITY`)
    ///
    /// Identity for ROUTER addressing. If None, a random UUID is generated.
    /// - Default: None (auto-generate)
    /// - Custom: Set for stable identity across reconnections
    pub routing_id: Option<bytes::Bytes>,

    /// Connect routing ID (`ZMQ_CONNECT_ROUTING_ID`)
    ///
    /// The routing id a ROUTER gives the peer of its first `connect`, in place
    /// of the identity that peer announces. It is used by that connection
    /// only; later connects route by the announced identity as usual.
    /// - Default: None
    pub connect_routing_id: Option<bytes::Bytes>,

    /// Custom handshake properties (`ZMQ_METADATA`)
    ///
    /// Sent in this socket's `READY` or `INITIATE` command (inside the box
    /// under CURVE), so the peer sees them as message metadata. libzmq expects
    /// names starting with `X-`. Peers reject a clear-text `READY` above 512
    /// bytes, so keep these short.
    /// - Default: none
    pub metadata: Vec<(String, bytes::Bytes)>,

    /// ROUTER mandatory mode (`ZMQ_ROUTER_MANDATORY`)
    ///
    /// - `false` (default): Silently drop messages to unknown peers
    /// - `true`: Return error when sending to unknown peer
    pub router_mandatory: bool,

    /// ROUTER handover mode (`ZMQ_ROUTER_HANDOVER`)
    ///
    /// - `false` (default): Disconnect old peer when new peer with same identity connects
    /// - `true`: Hand over pending messages to new peer with same identity
    pub router_handover: bool,

    /// XPUB verbose mode (`ZMQ_XPUB_VERBOSE`)
    ///
    /// - `false` (default): Only report new subscriptions
    /// - `true`: Report all subscription messages (including duplicates)
    pub xpub_verbose: bool,

    /// XPUB manual mode (`ZMQ_XPUB_MANUAL`)
    ///
    /// - `false` (default): subscriptions from peers take effect as they
    ///   arrive
    /// - `true`: they are only reported; `XPubSocket::subscribe` and
    ///   `unsubscribe` apply a prefix to the peer whose subscription message
    ///   was received last
    pub xpub_manual: bool,

    /// XPUB welcome message (`ZMQ_XPUB_WELCOME_MSG`)
    ///
    /// Message to send to new subscribers on connection.
    /// Useful for last value cache (LVC) patterns.
    pub xpub_welcome_msg: Option<bytes::Bytes>,

    /// XSUB verbose unsubscribe (`ZMQ_XSUB_VERBOSE_UNSUBSCRIBE`)
    ///
    /// - `false` (default): Don't send explicit unsubscribe messages
    /// - `true`: Send unsubscribe messages upstream
    pub xsub_verbose_unsubs: bool,

    /// Conflate messages (`ZMQ_CONFLATE`)
    ///
    /// - `false` (default): Queue all messages
    /// - `true`: Keep only the newest inbound message from each peer; each
    ///   arrival replaces the one waiting. A multipart message is kept or
    ///   replaced whole. [`recv_hwm`](Self::recv_hwm) no longer applies.
    ///   Publishers (PUB, XPUB) ignore it, since what they receive is
    ///   subscriptions, which must all be applied.
    pub conflate: bool,

    /// TCP keepalive (`ZMQ_TCP_KEEPALIVE`)
    ///
    /// - `-1` (default): Use OS default
    /// - `0`: Disable TCP keepalive
    /// - `1`: Enable TCP keepalive
    pub tcp_keepalive: i32,

    /// TCP keepalive count (`ZMQ_TCP_KEEPALIVE_CNT`)
    ///
    /// Number of keepalive probes before considering connection dead.
    /// - `-1` (default): Use OS default
    /// - `> 0`: Number of probes
    pub tcp_keepalive_cnt: i32,

    /// TCP keepalive idle (`ZMQ_TCP_KEEPALIVE_IDLE`)
    ///
    /// Time in seconds before starting keepalive probes.
    /// - `-1` (default): Use OS default
    /// - `> 0`: Idle time in seconds
    pub tcp_keepalive_idle: i32,

    /// TCP keepalive interval (`ZMQ_TCP_KEEPALIVE_INTVL`)
    ///
    /// Time in seconds between keepalive probes.
    /// - `-1` (default): Use OS default
    /// - `> 0`: Interval in seconds
    pub tcp_keepalive_intvl: i32,

    /// REQ correlate mode (`ZMQ_REQ_CORRELATE`)
    ///
    /// Match replies to requests using message envelope.
    /// - `false` (default): Accept any reply
    /// - `true`: Match reply envelope to request
    pub req_correlate: bool,

    /// REQ relaxed mode (`ZMQ_REQ_RELAXED`)
    ///
    /// Let a new request abandon the one still waiting for its reply.
    /// - `false` (default): a send while a reply is pending fails with
    ///   `InvalidState`
    /// - `true`: the pending request is given up and the new one is sent
    ///
    /// Set [`req_correlate`](Self::req_correlate) with it. Without it, as in
    /// libzmq, a late reply to the abandoned request can be returned as the
    /// reply to the new one.
    pub req_relaxed: bool,

    /// OS-level send buffer size (`ZMQ_SNDBUF`)
    ///
    /// Size of kernel send buffer. 0 = OS default.
    /// - Default: 0 (use OS default)
    pub sndbuf: i32,

    /// OS-level receive buffer size (`ZMQ_RCVBUF`)
    ///
    /// Size of kernel receive buffer. 0 = OS default.
    /// - Default: 0 (use OS default)
    pub rcvbuf: i32,

    /// Bind listeners with `SO_REUSEPORT` (Unix only).
    ///
    /// When `true`, a socket that creates its own listener binds it with
    /// `SO_REUSEPORT` so multiple acceptors can share one port with in-kernel
    /// load balancing (scaling accept for high-connection ROUTER/PULL/XPUB).
    /// - `false` (default): a single listener per address.
    pub reuse_port: bool,

    /// IP type of service (`ZMQ_TOS`)
    ///
    /// The value of the IPv4 `TOS` byte, or the IPv6 traffic class, on every
    /// TCP connection of the socket, for networks that queue by DSCP. It is
    /// set once a connection is established, so the packets of the TCP
    /// handshake itself go out unmarked.
    /// - Default: 0 (left as the OS has it)
    pub tos: u8,

    // --- Security Options ---
    /// PLAIN server mode (`ZMQ_PLAIN_SERVER`)
    ///
    /// Enable PLAIN authentication as server.
    /// - `false` (default): Client mode
    /// - `true`: Server mode (validate credentials)
    pub plain_server: bool,

    /// PLAIN username (`ZMQ_PLAIN_USERNAME`)
    ///
    /// Username for PLAIN authentication (client side).
    /// - Default: None (no authentication)
    pub plain_username: Option<String>,

    /// PLAIN password (`ZMQ_PLAIN_PASSWORD`)
    ///
    /// Password for PLAIN authentication (client side).
    /// - Default: None (no authentication)
    ///
    /// Wrapped in `Zeroizing` so the password buffer is scrubbed when these
    /// options (or a clone) are dropped. Read it via [`Self::plain_password`].
    pub plain_password: Option<zeroize::Zeroizing<String>>,

    /// CURVE server mode (`ZMQ_CURVE_SERVER`)
    ///
    /// Enable CURVE encryption as server.
    /// - `false` (default): Client mode
    /// - `true`: Server mode (provide server key)
    pub curve_server: bool,

    /// CURVE public key (`ZMQ_CURVE_PUBLICKEY`)
    ///
    /// Local public key for CURVE (32 bytes).
    /// - Default: None (no encryption)
    ///
    /// The handshake derives the public key from the secret key and refuses
    /// to start when this one is set and does not match it, which catches a
    /// public key paired with the wrong secret.
    pub curve_publickey: Option<[u8; 32]>,

    /// CURVE secret key (`ZMQ_CURVE_SECRETKEY`)
    ///
    /// Local secret key for CURVE (32 bytes).
    /// - Default: None (no encryption)
    ///
    /// Wrapped in `Zeroizing` so the key bytes are scrubbed when these options
    /// (or a clone) are dropped. Read it via [`Self::curve_secretkey`].
    pub curve_secretkey: Option<zeroize::Zeroizing<[u8; 32]>>,

    /// CURVE server key (`ZMQ_CURVE_SERVERKEY`)
    ///
    /// Server's public key for CURVE client (32 bytes).
    /// - Default: None (no encryption)
    /// - Client must set this to verify server identity
    pub curve_serverkey: Option<[u8; 32]>,

    /// ZAP domain (`ZMQ_ZAP_DOMAIN`)
    ///
    /// Security domain for ZAP authentication.
    /// - Default: "" (global domain)
    pub zap_domain: String,

    /// Subscriptions (`ZMQ_SUBSCRIBE`)
    ///
    /// Subscription filters for SUB/XSUB sockets.
    /// - Empty vec: No subscriptions (default) - won't receive any messages
    /// - vec![b""] or vec![`Bytes::new()`]: Subscribe to all messages
    /// - vec![b"topic1", b"topic2"]: Subscribe to specific topics
    ///
    /// Note: SUB sockets MUST subscribe to at least one topic to receive messages.
    pub subscriptions: Vec<bytes::Bytes>,

    /// Maximum reconnection attempts (`ZMQ_RECONNECT_STOP`)
    ///
    /// How many dials in a row may fail before a connected endpoint stops
    /// being dialed. A connection that succeeds resets the count.
    /// - `None`: Retry indefinitely (default, matches libzmq behaviour)
    /// - `Some(n)`: Stop after `n` consecutive failures; each failure is
    ///   reported as `SocketEvent::ConnectFailed`
    pub max_reconnect_attempts: Option<u32>,

    /// ZMTP heartbeat interval (`ZMQ_HEARTBEAT_IVL` = 75)
    ///
    /// How often to send PING heartbeat commands on an otherwise idle connection.
    /// - `None`: Disabled (default)
    /// - `Some(dur)`: Send PING every `dur` of inactivity
    pub heartbeat_ivl: Option<Duration>,

    /// ZMTP heartbeat TTL (`ZMQ_HEARTBEAT_TTL` = 76)
    ///
    /// Time-to-live for the remote peer's heartbeat (sent in PING command).
    /// The remote will disconnect if it doesn't receive a heartbeat within this interval.
    /// - `None`: Use `heartbeat_ivl` (default)
    /// - `Some(dur)`: Override TTL sent to peer
    pub heartbeat_ttl: Option<Duration>,

    /// ZMTP heartbeat timeout (`ZMQ_HEARTBEAT_TIMEOUT` = 77)
    ///
    /// How long to wait for a PONG reply before considering the connection dead.
    /// - `None`: Use `heartbeat_ivl` (default)
    /// - `Some(dur)`: Custom timeout (recommended: 2-5x `heartbeat_ivl`)
    pub heartbeat_timeout: Option<Duration>,

    /// STREAM connect/disconnect notifications (`ZMQ_STREAM_NOTIFY` = 73)
    ///
    /// Send empty notification frames on connect and disconnect.
    /// - `true` (default): Send notification frames
    /// - `false`: Suppress notification frames
    pub stream_notify: bool,

    /// Invert topic matching (`ZMQ_INVERT_MATCHING` = 74)
    ///
    /// Invert the subscription filter logic for PUB/SUB and XPUB/XSUB.
    /// - `false` (default): Deliver messages matching subscriptions
    /// - `true`: Deliver messages NOT matching any subscription
    pub invert_matching: bool,

    /// Write coalescing: batch multiple `send()` calls before writing to the kernel.
    ///
    /// When enabled, `send()` accumulates encoded messages in an internal buffer
    /// and only flushes to the kernel when `write_coalesce_threshold` bytes have
    /// accumulated or when `flush()` is called explicitly.
    ///
    /// - `false` (default): each `send()` writes immediately (lowest latency)
    /// - `true`: messages are batched (higher throughput for small messages)
    ///
    /// Always call `flush()` after the last `send()` in a burst to ensure
    /// all buffered data reaches the peer.
    pub write_coalescing: bool,

    /// Byte threshold at which the coalesce buffer is flushed automatically.
    ///
    /// Only relevant when `write_coalescing` is enabled. The internal send
    /// buffer is written to the kernel as a single syscall once it reaches
    /// this many bytes.
    ///
    /// On TCP that write carries a whole number of segments. The rest, less
    /// than one segment (which is up to 64 KB on loopback), goes out with the
    /// next batch while the sender keeps sending, rather than leaving as a
    /// short segment of its own. It never waits for a `flush()`: it is written
    /// as soon as the sending task yields to the runtime, along with anything
    /// queued since. A batch smaller than one segment is written whole.
    ///
    /// - Default: 65536 (64 KB) - one typical TCP segment on loopback
    pub write_coalesce_threshold: usize,

    /// Frame-body size at or above which the send path switches to a vectored
    /// write (`writev`) instead of copying the body into the userspace send
    /// buffer.
    ///
    /// For large frames the body copy into the coalescing buffer is the
    /// dominant per-byte cost on the core. Above this threshold the frame header
    /// and the refcounted `Bytes` body are written as an iovec, so the body is
    /// handed straight to the kernel with no intermediate copy. Small frames
    /// stay on the copy path, where a single `write` of one contiguous buffer
    /// beats the per-iovec bookkeeping.
    ///
    /// Applies with and without write coalescing, but not on a CURVE
    /// connection, where encryption writes every body into the outbound
    /// buffer anyway.
    ///
    /// - Default: 16384 (16 KiB). Measured on loopback against 8 and 32 KiB:
    ///   at 16 KiB bodies it beats 32 KiB by about half on coalesced
    ///   throughput and 3% on round-trip latency, and 8 KiB gains nothing
    ///   more. Tune for your hardware and message sizes.
    pub vectored_write_threshold: usize,
}

impl fmt::Debug for SocketOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketOptions")
            .field("read_buffer_size", &self.read_buffer_size())
            .field("recv_timeout", &self.recv_timeout)
            .field("send_timeout", &self.send_timeout)
            .field("handshake_timeout", &self.handshake_timeout)
            .field("linger", &self.linger)
            .field("write_stall", &self.write_stall)
            .field("reconnect_ivl", &self.reconnect_ivl)
            .field("reconnect_ivl_max", &self.reconnect_ivl_max)
            .field("connect_timeout", &self.connect_timeout)
            .field("recv_hwm", &self.recv_hwm)
            .field("send_hwm", &self.send_hwm)
            .field("max_msg_size", &self.max_msg_size)
            .field("routing_id", &self.routing_id)
            .field("connect_routing_id", &self.connect_routing_id)
            .field("router_mandatory", &self.router_mandatory)
            .field("router_handover", &self.router_handover)
            .field("xpub_verbose", &self.xpub_verbose)
            .field("xpub_manual", &self.xpub_manual)
            .field("xpub_welcome_msg", &self.xpub_welcome_msg)
            .field("xsub_verbose_unsubs", &self.xsub_verbose_unsubs)
            .field("conflate", &self.conflate)
            .field("tcp_keepalive", &self.tcp_keepalive)
            .field("tcp_keepalive_cnt", &self.tcp_keepalive_cnt)
            .field("tcp_keepalive_idle", &self.tcp_keepalive_idle)
            .field("tcp_keepalive_intvl", &self.tcp_keepalive_intvl)
            .field("req_correlate", &self.req_correlate)
            .field("req_relaxed", &self.req_relaxed)
            .field("sndbuf", &self.sndbuf)
            .field("rcvbuf", &self.rcvbuf)
            .field("reuse_port", &self.reuse_port)
            .field("tos", &self.tos)
            .field("plain_server", &self.plain_server)
            .field("plain_username", &self.plain_username)
            .field(
                "plain_password",
                &self.plain_password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("curve_server", &self.curve_server)
            .field("curve_publickey", &self.curve_publickey)
            .field(
                "curve_secretkey",
                &self.curve_secretkey.as_ref().map(|_| "[REDACTED]"),
            )
            .field("curve_serverkey", &self.curve_serverkey)
            .field("zap_domain", &self.zap_domain)
            .field("subscriptions", &self.subscriptions)
            .field("max_reconnect_attempts", &self.max_reconnect_attempts)
            .field("heartbeat_ivl", &self.heartbeat_ivl)
            .field("heartbeat_ttl", &self.heartbeat_ttl)
            .field("heartbeat_timeout", &self.heartbeat_timeout)
            .field("stream_notify", &self.stream_notify)
            .field("invert_matching", &self.invert_matching)
            .field("write_coalescing", &self.write_coalescing)
            .field("write_coalesce_threshold", &self.write_coalesce_threshold)
            .field("vectored_write_threshold", &self.vectored_write_threshold)
            .finish()
    }
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            recv_timeout: None, // Block indefinitely
            send_timeout: None, // Block indefinitely
            handshake_timeout: Duration::from_secs(30),
            linger: Some(Duration::from_secs(30)), // Wait 30s for pending messages
            write_stall: Some(Duration::from_millis(50)),
            reconnect_ivl: Duration::from_millis(100),
            reconnect_ivl_max: Duration::ZERO, // No maximum
            connect_timeout: Duration::ZERO,   // Use OS default
            recv_hwm: 1000,
            send_hwm: 1000,
            max_msg_size: None, // 64 MiB per frame
            // 32 KiB read batch. Larger batches cut recv syscalls on bulk
            // transfers roughly in proportion to their size (measured over a
            // 64 MiB PUSH/PULL flow: 8 KiB -> 8379 recvfrom, 16 KiB -> 4178,
            // 32 KiB -> 2104, 64 KiB -> 1037), but every peer keeps a buffer
            // this large, so the default stays at half the 64 KiB ceiling.
            read_buffer_size: 32768,
            routing_id: None,
            connect_routing_id: None,
            metadata: Vec::new(),
            router_mandatory: false,
            router_handover: false,
            xpub_verbose: false,
            xpub_manual: false,
            xpub_welcome_msg: None,
            xsub_verbose_unsubs: false,
            conflate: false,
            tcp_keepalive: -1,       // OS default
            tcp_keepalive_cnt: -1,   // OS default
            tcp_keepalive_idle: -1,  // OS default
            tcp_keepalive_intvl: -1, // OS default
            req_correlate: false,
            req_relaxed: false,
            sndbuf: 0, // OS default
            rcvbuf: 0, // OS default
            reuse_port: false,
            tos: 0, // OS default
            // Security
            plain_server: false,
            plain_username: None,
            plain_password: None,
            curve_server: false,
            curve_publickey: None,
            curve_secretkey: None,
            curve_serverkey: None,
            zap_domain: String::new(),    // Global domain
            subscriptions: Vec::new(),    // No subscriptions
            max_reconnect_attempts: None, // Retry indefinitely
            heartbeat_ivl: None,
            heartbeat_ttl: None,
            heartbeat_timeout: None,
            stream_notify: true,
            invert_matching: false,
            write_coalescing: false,
            write_coalesce_threshold: 65536,
            vectored_write_threshold: 16384,
        }
    }
}

impl SocketOptions {
    /// Create new socket options with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set receive timeout.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    /// use std::time::Duration;
    ///
    /// // Non-blocking receive
    /// let opts = SocketOptions::new().with_recv_timeout(Duration::ZERO);
    ///
    /// // 5 second timeout
    /// let opts = SocketOptions::new().with_recv_timeout(Duration::from_secs(5));
    /// ```
    pub const fn with_recv_timeout(mut self, timeout: Duration) -> Self {
        self.recv_timeout = Some(timeout);
        self
    }

    /// Set send timeout.
    pub const fn with_send_timeout(mut self, timeout: Duration) -> Self {
        self.send_timeout = Some(timeout);
        self
    }

    /// Set handshake timeout.
    pub const fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Set linger timeout.
    pub const fn with_linger(mut self, linger: Option<Duration>) -> Self {
        self.linger = linger;
        self
    }

    /// Set reconnection interval.
    pub const fn with_reconnect_ivl(mut self, ivl: Duration) -> Self {
        self.reconnect_ivl = ivl;
        self
    }

    /// Set maximum reconnection interval for exponential backoff.
    pub const fn with_reconnect_ivl_max(mut self, max: Duration) -> Self {
        self.reconnect_ivl_max = max;
        self
    }

    /// Set maximum number of reconnection attempts.
    ///
    /// `None` retries indefinitely (default); `Some(n)` gives up after n attempts.
    pub const fn with_max_reconnect_attempts(mut self, max: Option<u32>) -> Self {
        self.max_reconnect_attempts = max;
        self
    }

    /// Set the longest one dial may take (`ZMQ_CONNECT_TIMEOUT`). Zero leaves
    /// it to the OS.
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set heartbeat interval (`ZMQ_HEARTBEAT_IVL`).
    pub const fn with_heartbeat_ivl(mut self, ivl: Duration) -> Self {
        self.heartbeat_ivl = Some(ivl);
        self
    }

    /// Set heartbeat TTL (`ZMQ_HEARTBEAT_TTL`).
    pub const fn with_heartbeat_ttl(mut self, ttl: Duration) -> Self {
        self.heartbeat_ttl = Some(ttl);
        self
    }

    /// Set heartbeat timeout (`ZMQ_HEARTBEAT_TIMEOUT`).
    pub const fn with_heartbeat_timeout(mut self, timeout: Duration) -> Self {
        self.heartbeat_timeout = Some(timeout);
        self
    }

    /// Enable or disable STREAM connect/disconnect notifications (`ZMQ_STREAM_NOTIFY`).
    pub const fn with_stream_notify(mut self, notify: bool) -> Self {
        self.stream_notify = notify;
        self
    }

    /// Enable inverted topic matching (`ZMQ_INVERT_MATCHING`).
    pub const fn with_invert_matching(mut self, invert: bool) -> Self {
        self.invert_matching = invert;
        self
    }

    /// Enable or disable write coalescing.
    ///
    /// When enabled, consecutive `send()` calls accumulate in an internal buffer
    /// and are written to the kernel in one syscall, reducing per-message overhead
    /// for small-message workloads.  Call `flush()` after the last send in a burst.
    pub const fn with_write_coalescing(mut self, enabled: bool) -> Self {
        self.write_coalescing = enabled;
        self
    }

    /// Set the byte threshold at which the coalesce buffer flushes automatically.
    pub const fn with_write_coalesce_threshold(mut self, threshold: usize) -> Self {
        self.write_coalesce_threshold = threshold;
        self
    }

    /// Set the frame-body size at or above which the eager send path uses a
    /// vectored write (`writev`) instead of copying the body into the send
    /// buffer. See [`SocketOptions::vectored_write_threshold`]. Set to
    /// `usize::MAX` to disable vectored writes entirely.
    pub const fn with_vectored_write_threshold(mut self, threshold: usize) -> Self {
        self.vectored_write_threshold = threshold;
        self
    }

    /// Get the configured PLAIN password, if any.
    pub fn plain_password(&self) -> Option<&str> {
        self.plain_password.as_ref().map(|p| p.as_str())
    }

    /// Get the configured CURVE secret key, if any.
    pub fn curve_secretkey(&self) -> Option<&[u8; 32]> {
        self.curve_secretkey.as_deref()
    }

    /// Get the configured read buffer size after applying the 64 KiB read cap.
    pub const fn read_buffer_size(&self) -> usize {
        if self.read_buffer_size > crate::io::MAX_READ_SIZE {
            crate::io::MAX_READ_SIZE
        } else {
            self.read_buffer_size
        }
    }

    /// Set receive high water mark.
    pub const fn with_recv_hwm(mut self, hwm: usize) -> Self {
        self.recv_hwm = hwm;
        self
    }

    /// Set send high water mark.
    pub const fn with_send_hwm(mut self, hwm: usize) -> Self {
        self.send_hwm = hwm;
        self
    }

    /// Set the largest frame a peer may send. `None` is 64 MiB.
    pub const fn with_max_msg_size(mut self, size: Option<usize>) -> Self {
        self.max_msg_size = size;
        self
    }

    /// Set read buffer size.
    ///
    /// This is the size a connection starts at. While reads keep filling the
    /// buffer, a busy connection reads more at once, up to 64 KiB, and returns
    /// to this size when traffic quiets.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// // Small buffers for low latency
    /// let opts = SocketOptions::new().with_read_buffer_size(4096);
    ///
    /// // Large buffers for throughput
    /// let opts = SocketOptions::new().with_read_buffer_size(16384);
    /// ```
    pub const fn with_read_buffer_size(mut self, size: usize) -> Self {
        self.read_buffer_size = clamp_read_buffer_size(size);
        self
    }

    /// Set socket routing ID / identity.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    /// use bytes::Bytes;
    ///
    /// let opts = SocketOptions::new()
    ///     .with_routing_id(Bytes::from_static(b"worker-01"));
    /// ```
    pub fn with_routing_id(mut self, id: bytes::Bytes) -> Self {
        self.routing_id = Some(id);
        self
    }

    /// Announce a custom handshake property (`ZMQ_METADATA`), such as
    /// `X-Service`. See [`metadata`](Self::metadata).
    #[must_use]
    pub fn with_metadata(
        mut self,
        name: impl Into<String>,
        value: impl Into<bytes::Bytes>,
    ) -> Self {
        self.metadata.push((name.into(), value.into()));
        self
    }

    /// Set connect routing ID for the next connection.
    ///
    /// This option is consumed after each connect operation and must be set
    /// again for subsequent connections.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    /// use bytes::Bytes;
    ///
    /// let opts = SocketOptions::new()
    ///     .with_connect_routing_id(Bytes::from_static(b"client-001"));
    /// ```
    pub fn with_connect_routing_id(mut self, id: bytes::Bytes) -> Self {
        self.connect_routing_id = Some(id);
        self
    }

    /// Enable ROUTER mandatory mode.
    pub const fn with_router_mandatory(mut self, enabled: bool) -> Self {
        self.router_mandatory = enabled;
        self
    }

    /// Enable ROUTER handover mode.
    pub const fn with_router_handover(mut self, enabled: bool) -> Self {
        self.router_handover = enabled;
        self
    }

    /// Enable XPUB verbose mode.
    pub const fn with_xpub_verbose(mut self, enabled: bool) -> Self {
        self.xpub_verbose = enabled;
        self
    }

    /// Set how long an eager send waits on a peer that stopped reading; see
    /// [`write_stall`](Self::write_stall).
    pub const fn with_write_stall(mut self, window: Option<Duration>) -> Self {
        self.write_stall = window;
        self
    }

    /// Enable XPUB manual mode.
    pub const fn with_xpub_manual(mut self, enabled: bool) -> Self {
        self.xpub_manual = enabled;
        self
    }

    /// Set XPUB welcome message.
    pub fn with_xpub_welcome_msg(mut self, msg: bytes::Bytes) -> Self {
        self.xpub_welcome_msg = Some(msg);
        self
    }

    /// Enable XSUB verbose unsubscribe.
    pub const fn with_xsub_verbose_unsubs(mut self, enabled: bool) -> Self {
        self.xsub_verbose_unsubs = enabled;
        self
    }

    /// Keep only the newest inbound message from each peer. See
    /// [`conflate`](Self::conflate).
    pub const fn with_conflate(mut self, enabled: bool) -> Self {
        self.conflate = enabled;
        self
    }

    /// Set TCP keepalive mode.
    ///
    /// # Arguments
    ///
    /// * `mode` - `-1` for OS default, `0` to disable, `1` to enable
    pub const fn with_tcp_keepalive(mut self, mode: i32) -> Self {
        self.tcp_keepalive = mode;
        self
    }

    /// Set TCP keepalive count (number of probes before timeout).
    ///
    /// # Arguments
    ///
    /// * `count` - `-1` for OS default, `> 0` for specific count
    pub const fn with_tcp_keepalive_cnt(mut self, count: i32) -> Self {
        self.tcp_keepalive_cnt = count;
        self
    }

    /// Set TCP keepalive idle time (seconds before first probe).
    ///
    /// # Arguments
    ///
    /// * `seconds` - `-1` for OS default, `> 0` for specific idle time
    pub const fn with_tcp_keepalive_idle(mut self, seconds: i32) -> Self {
        self.tcp_keepalive_idle = seconds;
        self
    }

    /// Set TCP keepalive interval (seconds between probes).
    ///
    /// # Arguments
    ///
    /// * `seconds` - `-1` for OS default, `> 0` for specific interval
    pub const fn with_tcp_keepalive_intvl(mut self, seconds: i32) -> Self {
        self.tcp_keepalive_intvl = seconds;
        self
    }

    /// Enable REQ correlation mode (match replies to requests).
    pub const fn with_req_correlate(mut self, enabled: bool) -> Self {
        self.req_correlate = enabled;
        self
    }

    /// Enable REQ relaxed mode (allow multiple outstanding requests).
    pub const fn with_req_relaxed(mut self, enabled: bool) -> Self {
        self.req_relaxed = enabled;
        self
    }

    /// Set OS send buffer size (`ZMQ_SNDBUF`).
    pub const fn with_sndbuf(mut self, size: i32) -> Self {
        self.sndbuf = size;
        self
    }

    /// Set OS receive buffer size (`ZMQ_RCVBUF`).
    pub const fn with_rcvbuf(mut self, size: i32) -> Self {
        self.rcvbuf = size;
        self
    }

    /// Bind listeners with `SO_REUSEPORT` (Unix only) so multiple acceptors can
    /// share one port. See [`SocketOptions::reuse_port`].
    pub const fn with_reuse_port(mut self, enabled: bool) -> Self {
        self.reuse_port = enabled;
        self
    }

    /// Set the IPv4 `TOS` byte or IPv6 traffic class of the socket's TCP
    /// connections (`ZMQ_TOS`). See [`SocketOptions::tos`].
    pub const fn with_tos(mut self, tos: u8) -> Self {
        self.tos = tos;
        self
    }

    // --- Security Options ---

    /// Enable PLAIN server mode.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let opts = SocketOptions::new().with_plain_server(true);
    /// ```
    pub const fn with_plain_server(mut self, enabled: bool) -> Self {
        self.plain_server = enabled;
        self
    }

    /// Set PLAIN client credentials.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let opts = SocketOptions::new()
    ///     .with_plain_credentials("admin", "secret123");
    /// ```
    pub fn with_plain_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.plain_username = Some(username.into());
        self.plain_password = Some(zeroize::Zeroizing::new(password.into()));
        self
    }

    /// Enable CURVE server mode.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let opts = SocketOptions::new().with_curve_server(true);
    /// ```
    pub const fn with_curve_server(mut self, enabled: bool) -> Self {
        self.curve_server = enabled;
        self
    }

    /// Set this socket's CURVE key pair (public + secret), for a client or a
    /// server. The public key must be the one that belongs to the secret key;
    /// the handshake refuses a pair that does not match.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let public = [0u8; 32];  // Replace with actual key
    /// let secret = [0u8; 32];  // Replace with actual key
    /// let opts = SocketOptions::new().with_curve_keypair(public, secret);
    /// ```
    pub fn with_curve_keypair(mut self, publickey: [u8; 32], secretkey: [u8; 32]) -> Self {
        self.curve_publickey = Some(publickey);
        self.curve_secretkey = Some(zeroize::Zeroizing::new(secretkey));
        self
    }

    /// Set CURVE server public key (for client).
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let server_key = [0u8; 32];  // Server's public key
    /// let opts = SocketOptions::new().with_curve_serverkey(server_key);
    /// ```
    pub const fn with_curve_serverkey(mut self, serverkey: [u8; 32]) -> Self {
        self.curve_serverkey = Some(serverkey);
        self
    }

    /// Set ZAP domain for authentication.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let opts = SocketOptions::new().with_zap_domain("production");
    /// ```
    pub fn with_zap_domain(mut self, domain: impl Into<String>) -> Self {
        self.zap_domain = domain.into();
        self
    }

    /// Add a subscription filter for SUB/XSUB sockets (`ZMQ_SUBSCRIBE`).
    ///
    /// SUB sockets MUST subscribe to at least one topic to receive messages.
    /// An empty filter (b"" or `Bytes::new()`) subscribes to all messages.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    /// use bytes::Bytes;
    ///
    /// // Subscribe to all messages
    /// let opts = SocketOptions::new().with_subscribe(Bytes::new());
    ///
    /// // Subscribe to specific topics
    /// let opts = SocketOptions::new()
    ///     .with_subscribe(Bytes::from("weather."))
    ///     .with_subscribe(Bytes::from("stocks."));
    /// ```
    pub fn with_subscribe(mut self, filter: bytes::Bytes) -> Self {
        self.subscriptions.push(filter);
        self
    }

    /// Add multiple subscription filters for SUB/XSUB sockets.
    ///
    /// Convenience method to subscribe to multiple topics at once.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    /// use bytes::Bytes;
    ///
    /// let opts = SocketOptions::new()
    ///     .with_subscriptions(vec![
    ///         Bytes::from("weather."),
    ///         Bytes::from("stocks."),
    ///     ]);
    /// ```
    pub fn with_subscriptions(mut self, filters: Vec<bytes::Bytes>) -> Self {
        self.subscriptions.extend(filters);
        self
    }

    // --- Query Methods ---

    /// Check if receive operation should be non-blocking.
    pub const fn is_recv_nonblocking(&self) -> bool {
        matches!(self.recv_timeout, Some(d) if d.is_zero())
    }

    /// Check if send operation should be non-blocking.
    pub const fn is_send_nonblocking(&self) -> bool {
        matches!(self.send_timeout, Some(d) if d.is_zero())
    }

    /// Validate routing ID for use with ROUTER sockets.
    ///
    /// ROUTER socket identities must:
    /// - Be 1-255 bytes long
    /// - Not start with null byte (0x00) which is reserved for auto-generated IDs
    pub fn validate_router_identity(id: &[u8]) -> std::io::Result<()> {
        if id.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "routing ID cannot be empty",
            ));
        }

        if id.len() > 255 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("routing ID cannot exceed 255 bytes (got {})", id.len()),
            ));
        }

        if id[0] == 0x00 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "routing ID cannot start with null byte (reserved for auto-generated IDs)",
            ));
        }

        Ok(())
    }

    /// Validate general routing ID (for DEALER, REQ, REP).
    ///
    /// Less strict than ROUTER identities - allows null prefix.
    pub fn validate_routing_id(id: &[u8]) -> std::io::Result<()> {
        if id.len() > 255 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("routing ID cannot exceed 255 bytes (got {})", id.len()),
            ));
        }
        Ok(())
    }

    /// Get the current reconnection interval with exponential backoff.
    ///
    /// Returns the interval to use, considering exponential backoff
    /// and the maximum interval setting.
    pub fn next_reconnect_ivl(&self, attempt: u32) -> Duration {
        // Match reconnect::ReconnectState::next_delay so the two do not diverge:
        // max == 0 disables exponential backoff, and a max at or below the base
        // leaves no room to grow. Both cases hold at the base interval rather
        // than (as before) clamping the growing value down to a smaller max.
        if self.reconnect_ivl_max.is_zero() || self.reconnect_ivl_max <= self.reconnect_ivl {
            return self.reconnect_ivl;
        }

        // Calculate exponential backoff: base * 2^attempt
        let backoff = self
            .reconnect_ivl
            .saturating_mul(2u32.saturating_pow(attempt));

        // Cap at maximum interval
        backoff.min(self.reconnect_ivl_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_options() {
        let opts = SocketOptions::default();
        assert!(opts.recv_timeout.is_none());
        assert!(opts.send_timeout.is_none());
        assert_eq!(opts.handshake_timeout, Duration::from_secs(30));
        assert_eq!(opts.reconnect_ivl, Duration::from_millis(100));
        assert_eq!(opts.recv_hwm, 1000);
        assert_eq!(opts.send_hwm, 1000);
    }

    #[test]
    fn test_builder_pattern() {
        let opts = SocketOptions::new()
            .with_recv_timeout(Duration::from_secs(5))
            .with_send_timeout(Duration::from_secs(10))
            .with_recv_hwm(2000);

        assert_eq!(opts.recv_timeout, Some(Duration::from_secs(5)));
        assert_eq!(opts.send_timeout, Some(Duration::from_secs(10)));
        assert_eq!(opts.recv_hwm, 2000);
    }

    #[test]
    fn test_nonblocking_checks() {
        let blocking = SocketOptions::new();
        assert!(!blocking.is_recv_nonblocking());
        assert!(!blocking.is_send_nonblocking());

        let nonblocking = SocketOptions::new()
            .with_recv_timeout(Duration::ZERO)
            .with_send_timeout(Duration::ZERO);
        assert!(nonblocking.is_recv_nonblocking());
        assert!(nonblocking.is_send_nonblocking());
    }

    #[test]
    fn test_exponential_backoff() {
        let opts = SocketOptions::new()
            .with_reconnect_ivl(Duration::from_millis(100))
            .with_reconnect_ivl_max(Duration::from_secs(10));

        // First attempt: 100ms
        assert_eq!(opts.next_reconnect_ivl(0), Duration::from_millis(100));

        // Second attempt: 200ms
        assert_eq!(opts.next_reconnect_ivl(1), Duration::from_millis(200));

        // Third attempt: 400ms
        assert_eq!(opts.next_reconnect_ivl(2), Duration::from_millis(400));

        // Eventually caps at 10s
        assert_eq!(opts.next_reconnect_ivl(10), Duration::from_secs(10));
    }

    #[test]
    fn test_no_exponential_backoff() {
        let opts = SocketOptions::new().with_reconnect_ivl(Duration::from_millis(100));
        // reconnect_ivl_max is 0 by default

        // Always returns base interval
        assert_eq!(opts.next_reconnect_ivl(0), Duration::from_millis(100));
        assert_eq!(opts.next_reconnect_ivl(1), Duration::from_millis(100));
        assert_eq!(opts.next_reconnect_ivl(10), Duration::from_millis(100));
    }

    #[test]
    fn test_reconnect_ivl_max_below_base_holds_at_base() {
        // A max below the base leaves no room to grow: hold at base rather than
        // clamping down to the smaller max. This matches ReconnectState and is
        // the invariant next_reconnect_ivl previously violated.
        let opts = SocketOptions::new()
            .with_reconnect_ivl(Duration::from_millis(100))
            .with_reconnect_ivl_max(Duration::from_millis(50));

        for attempt in 0..5 {
            assert_eq!(
                opts.next_reconnect_ivl(attempt),
                Duration::from_millis(100),
                "max below base must hold at the base interval"
            );
        }
    }

    #[test]
    fn test_routing_id_validation() {
        // Valid ROUTER identities
        assert!(SocketOptions::validate_router_identity(b"client-001").is_ok());
        assert!(SocketOptions::validate_router_identity(&[0x01; 255]).is_ok());

        // Invalid: empty
        assert!(SocketOptions::validate_router_identity(b"").is_err());

        // Invalid: too long
        assert!(SocketOptions::validate_router_identity(&[0x01; 256]).is_err());

        // Invalid: starts with null byte
        assert!(SocketOptions::validate_router_identity(b"\x00client").is_err());
    }

    #[test]
    fn test_general_routing_id_validation() {
        // Valid
        assert!(SocketOptions::validate_routing_id(b"").is_ok()); // Empty allowed
        assert!(SocketOptions::validate_routing_id(b"\x00client").is_ok()); // Null prefix allowed
        assert!(SocketOptions::validate_routing_id(&[0x00; 255]).is_ok());

        // Invalid: too long
        assert!(SocketOptions::validate_routing_id(&[0x01; 256]).is_err());
    }

    #[test]
    fn test_connect_routing_id() {
        let opts =
            SocketOptions::new().with_connect_routing_id(bytes::Bytes::from_static(b"peer-123"));

        assert_eq!(
            opts.connect_routing_id,
            Some(bytes::Bytes::from_static(b"peer-123"))
        );
    }

    #[test]
    fn debug_output_redacts_security_options() {
        let opts = SocketOptions::new()
            .with_plain_credentials("alice", "super-secret-password")
            .with_curve_keypair([1u8; 32], [7u8; 32]);

        let debug = format!("{opts:?}");

        assert!(
            !debug.contains("super-secret-password"),
            "SocketOptions Debug output exposes the PLAIN password"
        );
        assert!(
            !debug.contains("curve_secretkey: Some([7, 7, 7"),
            "SocketOptions Debug output exposes the CURVE secret key"
        );
    }

    #[test]
    fn read_buffer_size_cannot_exceed_max_read_size() {
        let opts = SocketOptions::new().with_read_buffer_size(crate::io::MAX_READ_SIZE + 1);

        assert!(
            opts.read_buffer_size <= crate::io::MAX_READ_SIZE,
            "SocketOptions allowed a read buffer size larger than MAX_READ_SIZE"
        );
    }

    #[test]
    fn read_buffer_size_is_clamped_into_range() {
        // A zero read buffer would turn the read loop into a spin or a false
        // EOF.
        for size in [0, 1, 63, 64, 4096, crate::io::MAX_READ_SIZE + 1, usize::MAX] {
            let opts = SocketOptions::new().with_read_buffer_size(size);
            assert!(
                opts.read_buffer_size >= MIN_READ_BUFFER_SIZE
                    && opts.read_buffer_size <= crate::io::MAX_READ_SIZE,
                "with_read_buffer_size({size}) left the read buffer out of range"
            );
        }
    }

    #[test]
    fn test_router_options() {
        let opts = SocketOptions::new()
            .with_router_mandatory(true)
            .with_router_handover(true);

        assert!(opts.router_mandatory);
        assert!(opts.router_handover);
    }

    #[test]
    fn test_subscription_options() {
        // Test with_subscribe
        let opts = SocketOptions::new()
            .with_subscribe(bytes::Bytes::new()) // Subscribe to all
            .with_subscribe(bytes::Bytes::from("weather."))
            .with_subscribe(bytes::Bytes::from("stocks."));

        assert_eq!(opts.subscriptions.len(), 3);
        assert_eq!(opts.subscriptions[0], bytes::Bytes::new());
        assert_eq!(opts.subscriptions[1], bytes::Bytes::from("weather."));
        assert_eq!(opts.subscriptions[2], bytes::Bytes::from("stocks."));

        // Test with_subscriptions
        let opts2 = SocketOptions::new().with_subscriptions(vec![
            bytes::Bytes::from("topic1"),
            bytes::Bytes::from("topic2"),
        ]);

        assert_eq!(opts2.subscriptions.len(), 2);
    }
}
