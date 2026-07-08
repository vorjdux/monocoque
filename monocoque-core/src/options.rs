//! Socket configuration options
//!
//! This module provides configuration options for `ZeroMQ` sockets, similar to
//! libzmq's socket options (`zmq_setsockopt/zmq_getsockopt`).

use std::{fmt, time::Duration};

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
///     .with_buffer_sizes(16384, 16384);  // 16KB buffers for high-throughput
/// ```
#[derive(Clone)]
pub struct SocketOptions {
    /// Read buffer size (bytes)
    ///
    /// Size of arena-allocated buffer for receiving data.
    /// - Default: 8192 (8KB) - balanced for most workloads
    /// - Small (4KB): Low-latency with small messages (< 1KB)
    /// - Large (16KB): High-throughput with large messages (> 8KB)
    pub read_buffer_size: usize,

    /// Write buffer size (bytes)
    ///
    /// Initial capacity of `BytesMut` buffer for sending data.
    /// - Default: 8192 (8KB) - balanced for most workloads
    /// - Small (4KB): Low-latency with small messages
    /// - Large (16KB): High-throughput with large messages
    pub write_buffer_size: usize,

    /// Receive timeout (`ZMQ_RCVTIMEO`)
    ///
    /// Maximum time to wait for a receive operation.
    /// - `None`: Block indefinitely (default)
    /// - `Some(Duration::ZERO)`: Non-blocking (return immediately with EAGAIN)
    /// - `Some(duration)`: Wait up to duration before returning EAGAIN
    pub recv_timeout: Option<Duration>,

    /// Send timeout (`ZMQ_SNDTIMEO`)
    ///
    /// Maximum time to wait for a send operation.
    /// - `None`: Block indefinitely (default)
    /// - `Some(Duration::ZERO)`: Non-blocking (return immediately with EAGAIN)
    /// - `Some(duration)`: Wait up to duration before returning EAGAIN
    pub send_timeout: Option<Duration>,

    /// Handshake timeout (`ZMQ_HANDSHAKE_IVL`)
    ///
    /// Maximum time to complete ZMTP handshake after connection.
    /// - Default: 30 seconds
    /// - Set to `Duration::ZERO` to disable timeout
    pub handshake_timeout: Duration,

    /// Linger timeout (`ZMQ_LINGER`)
    ///
    /// Time to wait for pending messages to be sent before closing socket.
    /// - `None`: Close immediately, discard pending messages
    /// - `Some(Duration::ZERO)`: Same as None
    /// - `Some(duration)`: Wait up to duration for messages to be sent
    pub linger: Option<Duration>,

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
    /// Maximum time to wait for TCP connection to complete.
    /// - Default: 0 (use OS default)
    pub connect_timeout: Duration,

    /// High water mark for receiving (`ZMQ_RCVHWM`)
    ///
    /// Maximum number of messages to queue for receiving.
    /// When reached, socket will block or drop messages depending on socket type.
    /// - Default: 1000 messages
    pub recv_hwm: usize,

    /// High water mark for sending (`ZMQ_SNDHWM`)
    ///
    /// Maximum number of messages to queue for sending.
    /// When reached, socket will block or drop messages depending on socket type.
    /// - Default: 1000 messages
    pub send_hwm: usize,

    /// Enable immediate connect mode (`ZMQ_IMMEDIATE`)
    ///
    /// - `false` (default): Queue messages while connecting
    /// - `true`: Report error if no connection established
    pub immediate: bool,

    /// Maximum message size (`ZMQ_MAXMSGSIZE`)
    ///
    /// Maximum size of a single message in bytes.
    /// - `None`: No limit (default)
    /// - `Some(size)`: Reject messages larger than size
    pub max_msg_size: Option<usize>,

    /// Socket identity / routing ID (`ZMQ_ROUTING_ID` / `ZMQ_IDENTITY`)
    ///
    /// Identity for ROUTER addressing. If None, a random UUID is generated.
    /// - Default: None (auto-generate)
    /// - Custom: Set for stable identity across reconnections
    pub routing_id: Option<bytes::Bytes>,

    /// Connect routing ID (`ZMQ_CONNECT_ROUTING_ID`)
    ///
    /// Identity to assign to the next outgoing connection.
    /// Used by ROUTER sockets to assign a specific identity to a peer.
    /// - Default: None (auto-generate)
    /// - Custom: Assign explicit identity to next connection
    /// - Consumed after each connect operation
    pub connect_routing_id: Option<bytes::Bytes>,

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

    /// Probe ROUTER on connect (`ZMQ_PROBE_ROUTER`)
    ///
    /// - `false` (default): Normal operation
    /// - `true`: Send empty message on connect to probe ROUTER identity
    pub probe_router: bool,

    /// XPUB verbose mode (`ZMQ_XPUB_VERBOSE`)
    ///
    /// - `false` (default): Only report new subscriptions
    /// - `true`: Report all subscription messages (including duplicates)
    pub xpub_verbose: bool,

    /// XPUB manual mode (`ZMQ_XPUB_MANUAL`)
    ///
    /// - `false` (default): Automatic subscription management
    /// - `true`: Manual subscription control via `send()`
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
    /// - `true`: Keep only last message (overwrite queue)
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
    /// Allow multiple outstanding requests without strict alternation.
    /// - `false` (default): Strict send-recv-send-recv pattern
    /// - `true`: Allow send-send-recv-recv pattern
    pub req_relaxed: bool,

    /// Multicast rate in kilobits per second (`ZMQ_RATE`)
    ///
    /// Maximum send or receive data rate for multicast transports (PGM/EPGM).
    /// - Default: 100 kbps
    pub rate: i32,

    /// Multicast recovery interval (`ZMQ_RECOVERY_IVL`)
    ///
    /// Maximum time to recover lost messages on multicast transports.
    /// - Default: 10 seconds
    pub recovery_ivl: Duration,

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

    /// Multicast TTL (`ZMQ_MULTICAST_HOPS`)
    ///
    /// Time-to-live for multicast packets.
    /// - Default: 1 (local network only)
    pub multicast_hops: i32,

    /// IP Type of Service (`ZMQ_TOS`)
    ///
    /// Sets the `ToS` field in IP headers for `QoS`.
    /// - Default: 0 (normal service)
    pub tos: i32,

    /// Maximum multicast transmission unit (`ZMQ_MULTICAST_MAXTPDU`)
    ///
    /// Maximum transport data unit for multicast.
    /// - Default: 1500 bytes
    pub multicast_maxtpdu: i32,

    /// IPv6 support (`ZMQ_IPV6`)
    ///
    /// Enable IPv6 on socket.
    /// - `false` (default): IPv4 only
    /// - `true`: IPv6 support enabled
    pub ipv6: bool,

    /// Bind to device (`ZMQ_BINDTODEVICE`)
    ///
    /// Bind socket to specific network interface (Linux only).
    /// - Default: None (bind to all interfaces)
    pub bind_to_device: Option<String>,

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
    pub plain_password: Option<String>,

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
    pub curve_publickey: Option<[u8; 32]>,

    /// CURVE secret key (`ZMQ_CURVE_SECRETKEY`)
    ///
    /// Local secret key for CURVE (32 bytes).
    /// - Default: None (no encryption)
    pub curve_secretkey: Option<[u8; 32]>,

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

    /// Unsubscriptions (`ZMQ_UNSUBSCRIBE`)
    ///
    /// Subscription filters to remove for SUB/XSUB sockets.
    /// Applied after subscriptions during socket configuration.
    pub unsubscriptions: Vec<bytes::Bytes>,

    /// Maximum reconnection attempts (`ZMQ_RECONNECT_STOP`)
    ///
    /// Maximum number of times to attempt reconnection after a disconnect.
    /// - `None`: Retry indefinitely (default, matches libzmq behaviour)
    /// - `Some(n)`: Give up and return `NotConnected` after n attempts
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

    /// ROUTER raw mode (`ZMQ_ROUTER_RAW` = 41)
    ///
    /// Put ROUTER socket into raw mode (no ZMTP handshake, acts like STREAM).
    /// - `false` (default): Normal ZMTP routing
    /// - `true`: Raw TCP bridging mode
    pub router_raw: bool,

    /// STREAM connect/disconnect notifications (`ZMQ_STREAM_NOTIFY` = 73)
    ///
    /// Send empty notification frames on connect and disconnect.
    /// - `true` (default): Send notification frames
    /// - `false`: Suppress notification frames
    pub stream_notify: bool,

    /// XPUB no-drop mode (`ZMQ_XPUB_NODROP` = 69)
    ///
    /// - `false` (default): Drop messages silently when HWM is reached
    /// - `true`: Return error (`EAGAIN`) instead of dropping
    pub xpub_nodrop: bool,

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
    /// Only applies in eager mode (write coalescing disabled) and when the
    /// connection is not CURVE-encrypted (encryption must transform the body
    /// into a fresh buffer regardless).
    ///
    /// - Default: 32768 (32 KB) - the measured crossover on loopback below which
    ///   copying the body into one contiguous buffer beats a two-segment
    ///   `writev`; tune for your hardware and message sizes
    pub vectored_write_threshold: usize,
}

impl fmt::Debug for SocketOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketOptions")
            .field("read_buffer_size", &self.read_buffer_size())
            .field("write_buffer_size", &self.write_buffer_size)
            .field("recv_timeout", &self.recv_timeout)
            .field("send_timeout", &self.send_timeout)
            .field("handshake_timeout", &self.handshake_timeout)
            .field("linger", &self.linger)
            .field("reconnect_ivl", &self.reconnect_ivl)
            .field("reconnect_ivl_max", &self.reconnect_ivl_max)
            .field("connect_timeout", &self.connect_timeout)
            .field("recv_hwm", &self.recv_hwm)
            .field("send_hwm", &self.send_hwm)
            .field("immediate", &self.immediate)
            .field("max_msg_size", &self.max_msg_size)
            .field("routing_id", &self.routing_id)
            .field("connect_routing_id", &self.connect_routing_id)
            .field("router_mandatory", &self.router_mandatory)
            .field("router_handover", &self.router_handover)
            .field("probe_router", &self.probe_router)
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
            .field("rate", &self.rate)
            .field("recovery_ivl", &self.recovery_ivl)
            .field("sndbuf", &self.sndbuf)
            .field("rcvbuf", &self.rcvbuf)
            .field("multicast_hops", &self.multicast_hops)
            .field("tos", &self.tos)
            .field("multicast_maxtpdu", &self.multicast_maxtpdu)
            .field("ipv6", &self.ipv6)
            .field("bind_to_device", &self.bind_to_device)
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
            .field("unsubscriptions", &self.unsubscriptions)
            .field("max_reconnect_attempts", &self.max_reconnect_attempts)
            .field("heartbeat_ivl", &self.heartbeat_ivl)
            .field("heartbeat_ttl", &self.heartbeat_ttl)
            .field("heartbeat_timeout", &self.heartbeat_timeout)
            .field("router_raw", &self.router_raw)
            .field("stream_notify", &self.stream_notify)
            .field("xpub_nodrop", &self.xpub_nodrop)
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
            reconnect_ivl: Duration::from_millis(100),
            reconnect_ivl_max: Duration::ZERO, // No maximum
            connect_timeout: Duration::ZERO,   // Use OS default
            recv_hwm: 1000,
            send_hwm: 1000,
            immediate: false,
            max_msg_size: None,      // No limit
            read_buffer_size: 8192,  // 8KB - balanced default
            write_buffer_size: 8192, // 8KB - balanced default
            routing_id: None,
            connect_routing_id: None,
            router_mandatory: false,
            router_handover: false,
            probe_router: false,
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
            rate: 100, // 100 kbps
            recovery_ivl: Duration::from_secs(10),
            sndbuf: 0,               // OS default
            rcvbuf: 0,               // OS default
            multicast_hops: 1,       // Local network only
            tos: 0,                  // Normal service
            multicast_maxtpdu: 1500, // Standard MTU
            ipv6: false,             // IPv4 only
            bind_to_device: None,    // All interfaces
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
            unsubscriptions: Vec::new(),  // No unsubscriptions
            max_reconnect_attempts: None, // Retry indefinitely
            heartbeat_ivl: None,
            heartbeat_ttl: None,
            heartbeat_timeout: None,
            router_raw: false,
            stream_notify: true,
            xpub_nodrop: false,
            invert_matching: false,
            write_coalescing: false,
            write_coalesce_threshold: 65536,
            vectored_write_threshold: 32768,
        }
    }
}

impl SocketOptions {
    /// Create new socket options with default values (8KB buffers).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create socket options optimized for small messages (< 1KB).
    ///
    /// Sets 4KB buffers, suitable for low-latency request-reply patterns.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let opts = SocketOptions::small();  // 4KB buffers for REQ/REP
    /// ```
    #[must_use]
    pub fn small() -> Self {
        Self {
            read_buffer_size: 4096,
            write_buffer_size: 4096,
            ..Self::default()
        }
    }

    /// Create socket options optimized for large messages (> 8KB).
    ///
    /// Sets 16KB buffers, suitable for high-throughput async patterns.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// let opts = SocketOptions::large();  // 16KB buffers for DEALER/ROUTER
    /// ```
    #[must_use]
    pub fn large() -> Self {
        Self {
            read_buffer_size: 16384,
            write_buffer_size: 16384,
            ..Self::default()
        }
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

    /// Set connection timeout.
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

    /// Enable or disable ROUTER raw mode (`ZMQ_ROUTER_RAW`).
    pub const fn with_router_raw(mut self, raw: bool) -> Self {
        self.router_raw = raw;
        self
    }

    /// Enable or disable STREAM connect/disconnect notifications (`ZMQ_STREAM_NOTIFY`).
    pub const fn with_stream_notify(mut self, notify: bool) -> Self {
        self.stream_notify = notify;
        self
    }

    /// Enable XPUB no-drop mode (`ZMQ_XPUB_NODROP`).
    pub const fn with_xpub_nodrop(mut self, nodrop: bool) -> Self {
        self.xpub_nodrop = nodrop;
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
        self.plain_password.as_deref()
    }

    /// Get the configured CURVE secret key, if any.
    pub const fn curve_secretkey(&self) -> Option<&[u8; 32]> {
        self.curve_secretkey.as_ref()
    }

    /// Get the configured read buffer size after applying the read-slab cap.
    pub const fn read_buffer_size(&self) -> usize {
        if self.read_buffer_size > crate::io::READ_SLAB_SIZE {
            crate::io::READ_SLAB_SIZE
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

    /// Enable or disable immediate mode.
    pub const fn with_immediate(mut self, immediate: bool) -> Self {
        self.immediate = immediate;
        self
    }

    /// Set maximum message size.
    pub const fn with_max_msg_size(mut self, size: Option<usize>) -> Self {
        self.max_msg_size = size;
        self
    }

    /// Set read buffer size.
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
        self.read_buffer_size = if size > crate::io::READ_SLAB_SIZE {
            crate::io::READ_SLAB_SIZE
        } else {
            size
        };
        self
    }

    /// Set write buffer size.
    pub const fn with_write_buffer_size(mut self, size: usize) -> Self {
        self.write_buffer_size = size;
        self
    }

    /// Set both read and write buffer sizes (convenience method).
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    ///
    /// // Small buffers for both
    /// let opts = SocketOptions::new().with_buffer_sizes(4096, 4096);
    /// ```
    pub const fn with_buffer_sizes(mut self, read_size: usize, write_size: usize) -> Self {
        self.read_buffer_size = if read_size > crate::io::READ_SLAB_SIZE {
            crate::io::READ_SLAB_SIZE
        } else {
            read_size
        };
        self.write_buffer_size = write_size;
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

    /// Enable ROUTER probe on connect.
    pub const fn with_probe_router(mut self, enabled: bool) -> Self {
        self.probe_router = enabled;
        self
    }

    /// Enable XPUB verbose mode.
    pub const fn with_xpub_verbose(mut self, enabled: bool) -> Self {
        self.xpub_verbose = enabled;
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

    /// Enable message conflation (keep only last message).
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

    /// Set multicast rate (`ZMQ_RATE`).
    pub const fn with_rate(mut self, rate: i32) -> Self {
        self.rate = rate;
        self
    }

    /// Set multicast recovery interval (`ZMQ_RECOVERY_IVL`).
    pub const fn with_recovery_ivl(mut self, interval: Duration) -> Self {
        self.recovery_ivl = interval;
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

    /// Set multicast TTL/hops (`ZMQ_MULTICAST_HOPS`).
    pub const fn with_multicast_hops(mut self, hops: i32) -> Self {
        self.multicast_hops = hops;
        self
    }

    /// Set IP Type of Service (`ZMQ_TOS`).
    pub const fn with_tos(mut self, tos: i32) -> Self {
        self.tos = tos;
        self
    }

    /// Set multicast maximum TPU (`ZMQ_MULTICAST_MAXTPDU`).
    pub const fn with_multicast_maxtpdu(mut self, mtu: i32) -> Self {
        self.multicast_maxtpdu = mtu;
        self
    }

    /// Enable IPv6 support (`ZMQ_IPV6`).
    pub const fn with_ipv6(mut self, enabled: bool) -> Self {
        self.ipv6 = enabled;
        self
    }

    /// Bind to specific device (`ZMQ_BINDTODEVICE`) - Linux only.
    pub fn with_bind_to_device(mut self, device: impl Into<String>) -> Self {
        self.bind_to_device = Some(device.into());
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
        self.plain_password = Some(password.into());
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

    /// Set CURVE client keys (public + secret).
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
    pub const fn with_curve_keypair(mut self, publickey: [u8; 32], secretkey: [u8; 32]) -> Self {
        self.curve_publickey = Some(publickey);
        self.curve_secretkey = Some(secretkey);
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

    /// Add an unsubscription filter for SUB/XSUB sockets (`ZMQ_UNSUBSCRIBE`).
    ///
    /// Removes a previously added subscription filter.
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::options::SocketOptions;
    /// use bytes::Bytes;
    ///
    /// let opts = SocketOptions::new()
    ///     .with_subscribe(Bytes::new())  // Subscribe to all
    ///     .with_unsubscribe(Bytes::from("admin.")); // Except admin topics
    /// ```
    pub fn with_unsubscribe(mut self, filter: bytes::Bytes) -> Self {
        self.unsubscriptions.push(filter);
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
        if self.reconnect_ivl_max.is_zero() {
            // No exponential backoff, always use base interval
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
    fn read_buffer_size_cannot_exceed_read_slab_size() {
        let opts = SocketOptions::new().with_read_buffer_size(crate::io::READ_SLAB_SIZE + 1);

        assert!(
            opts.read_buffer_size <= crate::io::READ_SLAB_SIZE,
            "SocketOptions allowed a read buffer size larger than the read slab"
        );
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

        // Test with_unsubscribe
        let opts3 = opts.with_unsubscribe(bytes::Bytes::from("admin."));
        assert_eq!(opts3.unsubscriptions.len(), 1);
        assert_eq!(opts3.unsubscriptions[0], bytes::Bytes::from("admin."));
    }
}
