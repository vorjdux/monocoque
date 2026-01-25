//! Socket configuration options
//!
//! This module provides configuration options for ZeroMQ sockets, similar to
//! libzmq's socket options (zmq_setsockopt/zmq_getsockopt).

use std::time::Duration;

/// Socket configuration options.
///
/// These options control socket behavior including timeouts, buffer sizes,
/// and reliability features. This struct consolidates all socket configuration
/// in one place, following the MongoDB Rust driver pattern.
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
#[derive(Debug, Clone)]
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
    /// Initial capacity of BytesMut buffer for sending data.
    /// - Default: 8192 (8KB) - balanced for most workloads
    /// - Small (4KB): Low-latency with small messages
    /// - Large (16KB): High-throughput with large messages
    pub write_buffer_size: usize,

    /// Receive timeout (ZMQ_RCVTIMEO)
    ///
    /// Maximum time to wait for a receive operation.
    /// - `None`: Block indefinitely (default)
    /// - `Some(Duration::ZERO)`: Non-blocking (return immediately with EAGAIN)
    /// - `Some(duration)`: Wait up to duration before returning EAGAIN
    pub recv_timeout: Option<Duration>,

    /// Send timeout (ZMQ_SNDTIMEO)
    ///
    /// Maximum time to wait for a send operation.
    /// - `None`: Block indefinitely (default)
    /// - `Some(Duration::ZERO)`: Non-blocking (return immediately with EAGAIN)
    /// - `Some(duration)`: Wait up to duration before returning EAGAIN
    pub send_timeout: Option<Duration>,

    /// Handshake timeout (ZMQ_HANDSHAKE_IVL)
    ///
    /// Maximum time to complete ZMTP handshake after connection.
    /// - Default: 30 seconds
    /// - Set to `Duration::ZERO` to disable timeout
    pub handshake_timeout: Duration,

    /// Linger timeout (ZMQ_LINGER)
    ///
    /// Time to wait for pending messages to be sent before closing socket.
    /// - `None`: Close immediately, discard pending messages
    /// - `Some(Duration::ZERO)`: Same as None
    /// - `Some(duration)`: Wait up to duration for messages to be sent
    pub linger: Option<Duration>,

    /// Reconnect interval (ZMQ_RECONNECT_IVL)
    ///
    /// Initial reconnection delay after connection loss.
    /// - Default: 100ms
    /// - Use with `reconnect_ivl_max` for exponential backoff
    pub reconnect_ivl: Duration,

    /// Maximum reconnect interval (ZMQ_RECONNECT_IVL_MAX)
    ///
    /// Maximum reconnection delay for exponential backoff.
    /// - Default: 0 (no maximum, use `reconnect_ivl` always)
    /// - When > 0: Doubles `reconnect_ivl` up to this value
    pub reconnect_ivl_max: Duration,

    /// Connection timeout (ZMQ_CONNECT_TIMEOUT)
    ///
    /// Maximum time to wait for TCP connection to complete.
    /// - Default: 0 (use OS default)
    pub connect_timeout: Duration,

    /// High water mark for receiving (ZMQ_RCVHWM)
    ///
    /// Maximum number of messages to queue for receiving.
    /// When reached, socket will block or drop messages depending on socket type.
    /// - Default: 1000 messages
    pub recv_hwm: usize,

    /// High water mark for sending (ZMQ_SNDHWM)
    ///
    /// Maximum number of messages to queue for sending.
    /// When reached, socket will block or drop messages depending on socket type.
    /// - Default: 1000 messages
    pub send_hwm: usize,

    /// Enable immediate connect mode (ZMQ_IMMEDIATE)
    ///
    /// - `false` (default): Queue messages while connecting
    /// - `true`: Report error if no connection established
    pub immediate: bool,

    /// Maximum message size (ZMQ_MAXMSGSIZE)
    ///
    /// Maximum size of a single message in bytes.
    /// - `None`: No limit (default)
    /// - `Some(size)`: Reject messages larger than size
    pub max_msg_size: Option<usize>,

    /// Socket identity / routing ID (ZMQ_ROUTING_ID / ZMQ_IDENTITY)
    ///
    /// Identity for ROUTER addressing. If None, a random UUID is generated.
    /// - Default: None (auto-generate)
    /// - Custom: Set for stable identity across reconnections
    pub routing_id: Option<bytes::Bytes>,

    /// Connect routing ID (ZMQ_CONNECT_ROUTING_ID)
    ///
    /// Identity to assign to the next outgoing connection.
    /// Used by ROUTER sockets to assign a specific identity to a peer.
    /// - Default: None (auto-generate)
    /// - Custom: Assign explicit identity to next connection
    /// - Consumed after each connect operation
    pub connect_routing_id: Option<bytes::Bytes>,

    /// ROUTER mandatory mode (ZMQ_ROUTER_MANDATORY)
    ///
    /// - `false` (default): Silently drop messages to unknown peers
    /// - `true`: Return error when sending to unknown peer
    pub router_mandatory: bool,

    /// ROUTER handover mode (ZMQ_ROUTER_HANDOVER)
    ///
    /// - `false` (default): Disconnect old peer when new peer with same identity connects
    /// - `true`: Hand over pending messages to new peer with same identity
    pub router_handover: bool,

    /// Probe ROUTER on connect (ZMQ_PROBE_ROUTER)
    ///
    /// - `false` (default): Normal operation
    /// - `true`: Send empty message on connect to probe ROUTER identity
    pub probe_router: bool,

    /// XPUB verbose mode (ZMQ_XPUB_VERBOSE)
    ///
    /// - `false` (default): Only report new subscriptions
    /// - `true`: Report all subscription messages (including duplicates)
    pub xpub_verbose: bool,

    /// XPUB manual mode (ZMQ_XPUB_MANUAL)
    ///
    /// - `false` (default): Automatic subscription management
    /// - `true`: Manual subscription control via send()
    pub xpub_manual: bool,

    /// XPUB welcome message (ZMQ_XPUB_WELCOME_MSG)
    ///
    /// Message to send to new subscribers on connection.
    /// Useful for last value cache (LVC) patterns.
    pub xpub_welcome_msg: Option<bytes::Bytes>,

    /// XSUB verbose unsubscribe (ZMQ_XSUB_VERBOSE_UNSUBSCRIBE)
    ///
    /// - `false` (default): Don't send explicit unsubscribe messages
    /// - `true`: Send unsubscribe messages upstream
    pub xsub_verbose_unsubs: bool,

    /// Conflate messages (ZMQ_CONFLATE)
    ///
    /// - `false` (default): Queue all messages
    /// - `true`: Keep only last message (overwrite queue)
    pub conflate: bool,

    /// TCP keepalive (ZMQ_TCP_KEEPALIVE)
    ///
    /// - `-1` (default): Use OS default
    /// - `0`: Disable TCP keepalive
    /// - `1`: Enable TCP keepalive
    pub tcp_keepalive: i32,

    /// TCP keepalive count (ZMQ_TCP_KEEPALIVE_CNT)
    ///
    /// Number of keepalive probes before considering connection dead.
    /// - `-1` (default): Use OS default
    /// - `> 0`: Number of probes
    pub tcp_keepalive_cnt: i32,

    /// TCP keepalive idle (ZMQ_TCP_KEEPALIVE_IDLE)
    ///
    /// Time in seconds before starting keepalive probes.
    /// - `-1` (default): Use OS default
    /// - `> 0`: Idle time in seconds
    pub tcp_keepalive_idle: i32,

    /// TCP keepalive interval (ZMQ_TCP_KEEPALIVE_INTVL)
    ///
    /// Time in seconds between keepalive probes.
    /// - `-1` (default): Use OS default
    /// - `> 0`: Interval in seconds
    pub tcp_keepalive_intvl: i32,

    /// REQ correlate mode (ZMQ_REQ_CORRELATE)
    ///
    /// Match replies to requests using message envelope.
    /// - `false` (default): Accept any reply
    /// - `true`: Match reply envelope to request
    pub req_correlate: bool,

    /// REQ relaxed mode (ZMQ_REQ_RELAXED)
    ///
    /// Allow multiple outstanding requests without strict alternation.
    /// - `false` (default): Strict send-recv-send-recv pattern
    /// - `true`: Allow send-send-recv-recv pattern
    pub req_relaxed: bool,

    /// Multicast rate in kilobits per second (ZMQ_RATE)
    ///
    /// Maximum send or receive data rate for multicast transports (PGM/EPGM).
    /// - Default: 100 kbps
    pub rate: i32,

    /// Multicast recovery interval (ZMQ_RECOVERY_IVL)
    ///
    /// Maximum time to recover lost messages on multicast transports.
    /// - Default: 10 seconds
    pub recovery_ivl: Duration,

    /// OS-level send buffer size (ZMQ_SNDBUF)
    ///
    /// Size of kernel send buffer. 0 = OS default.
    /// - Default: 0 (use OS default)
    pub sndbuf: i32,

    /// OS-level receive buffer size (ZMQ_RCVBUF)
    ///
    /// Size of kernel receive buffer. 0 = OS default.
    /// - Default: 0 (use OS default)
    pub rcvbuf: i32,

    /// Multicast TTL (ZMQ_MULTICAST_HOPS)
    ///
    /// Time-to-live for multicast packets.
    /// - Default: 1 (local network only)
    pub multicast_hops: i32,

    /// IP Type of Service (ZMQ_TOS)
    ///
    /// Sets the ToS field in IP headers for QoS.
    /// - Default: 0 (normal service)
    pub tos: i32,

    /// Maximum multicast transmission unit (ZMQ_MULTICAST_MAXTPDU)
    ///
    /// Maximum transport data unit for multicast.
    /// - Default: 1500 bytes
    pub multicast_maxtpdu: i32,

    /// IPv6 support (ZMQ_IPV6)
    ///
    /// Enable IPv6 on socket.
    /// - `false` (default): IPv4 only
    /// - `true`: IPv6 support enabled
    pub ipv6: bool,

    /// Bind to device (ZMQ_BINDTODEVICE)
    ///
    /// Bind socket to specific network interface (Linux only).
    /// - Default: None (bind to all interfaces)
    pub bind_to_device: Option<String>,

    // --- Security Options ---

    /// PLAIN server mode (ZMQ_PLAIN_SERVER)
    ///
    /// Enable PLAIN authentication as server.
    /// - `false` (default): Client mode
    /// - `true`: Server mode (validate credentials)
    pub plain_server: bool,

    /// PLAIN username (ZMQ_PLAIN_USERNAME)
    ///
    /// Username for PLAIN authentication (client side).
    /// - Default: None (no authentication)
    pub plain_username: Option<String>,

    /// PLAIN password (ZMQ_PLAIN_PASSWORD)
    ///
    /// Password for PLAIN authentication (client side).
    /// - Default: None (no authentication)
    pub plain_password: Option<String>,

    /// CURVE server mode (ZMQ_CURVE_SERVER)
    ///
    /// Enable CURVE encryption as server.
    /// - `false` (default): Client mode
    /// - `true`: Server mode (provide server key)
    pub curve_server: bool,

    /// CURVE public key (ZMQ_CURVE_PUBLICKEY)
    ///
    /// Local public key for CURVE (32 bytes).
    /// - Default: None (no encryption)
    pub curve_publickey: Option<[u8; 32]>,

    /// CURVE secret key (ZMQ_CURVE_SECRETKEY)
    ///
    /// Local secret key for CURVE (32 bytes).
    /// - Default: None (no encryption)
    pub curve_secretkey: Option<[u8; 32]>,

    /// CURVE server key (ZMQ_CURVE_SERVERKEY)
    ///
    /// Server's public key for CURVE client (32 bytes).
    /// - Default: None (no encryption)
    /// - Client must set this to verify server identity
    pub curve_serverkey: Option<[u8; 32]>,

    /// ZAP domain (ZMQ_ZAP_DOMAIN)
    ///
    /// Security domain for ZAP authentication.
    /// - Default: "" (global domain)
    pub zap_domain: String,

    /// Subscriptions (ZMQ_SUBSCRIBE)
    ///
    /// Subscription filters for SUB/XSUB sockets.
    /// - Empty vec: No subscriptions (default) - won't receive any messages
    /// - vec![b""] or vec![Bytes::new()]: Subscribe to all messages
    /// - vec![b"topic1", b"topic2"]: Subscribe to specific topics
    ///
    /// Note: SUB sockets MUST subscribe to at least one topic to receive messages.
    pub subscriptions: Vec<bytes::Bytes>,

    /// Unsubscriptions (ZMQ_UNSUBSCRIBE)
    ///
    /// Subscription filters to remove for SUB/XSUB sockets.
    /// Applied after subscriptions during socket configuration.
    pub unsubscriptions: Vec<bytes::Bytes>,
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
            max_msg_size: None, // No limit
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
            tcp_keepalive: -1,      // OS default
            tcp_keepalive_cnt: -1,  // OS default
            tcp_keepalive_idle: -1, // OS default
            tcp_keepalive_intvl: -1, // OS default
            req_correlate: false,
            req_relaxed: false,
            rate: 100,              // 100 kbps
            recovery_ivl: Duration::from_secs(10),
            sndbuf: 0,              // OS default
            rcvbuf: 0,              // OS default
            multicast_hops: 1,      // Local network only
            tos: 0,                 // Normal service
            multicast_maxtpdu: 1500, // Standard MTU
            ipv6: false,            // IPv4 only
            bind_to_device: None,   // All interfaces
            // Security
            plain_server: false,
            plain_username: None,
            plain_password: None,
            curve_server: false,
            curve_publickey: None,
            curve_secretkey: None,
            curve_serverkey: None,
            zap_domain: String::new(), // Global domain
            subscriptions: Vec::new(),     // No subscriptions
            unsubscriptions: Vec::new(),   // No unsubscriptions
        }
    }
}

impl SocketOptions {
    /// Create new socket options with default values (8KB buffers).
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
    pub fn with_recv_timeout(mut self, timeout: Duration) -> Self {
        self.recv_timeout = Some(timeout);
        self
    }

    /// Set send timeout.
    pub fn with_send_timeout(mut self, timeout: Duration) -> Self {
        self.send_timeout = Some(timeout);
        self
    }

    /// Set handshake timeout.
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Set linger timeout.
    pub fn with_linger(mut self, linger: Option<Duration>) -> Self {
        self.linger = linger;
        self
    }

    /// Set reconnection interval.
    pub fn with_reconnect_ivl(mut self, ivl: Duration) -> Self {
        self.reconnect_ivl = ivl;
        self
    }

    /// Set maximum reconnection interval for exponential backoff.
    pub fn with_reconnect_ivl_max(mut self, max: Duration) -> Self {
        self.reconnect_ivl_max = max;
        self
    }

    /// Set connection timeout.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set receive high water mark.
    pub fn with_recv_hwm(mut self, hwm: usize) -> Self {
        self.recv_hwm = hwm;
        self
    }

    /// Set send high water mark.
    pub fn with_send_hwm(mut self, hwm: usize) -> Self {
        self.send_hwm = hwm;
        self
    }

    /// Enable or disable immediate mode.
    pub fn with_immediate(mut self, immediate: bool) -> Self {
        self.immediate = immediate;
        self
    }

    /// Set maximum message size.
    pub fn with_max_msg_size(mut self, size: Option<usize>) -> Self {
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
    pub fn with_read_buffer_size(mut self, size: usize) -> Self {
        self.read_buffer_size = size;
        self
    }

    /// Set write buffer size.
    pub fn with_write_buffer_size(mut self, size: usize) -> Self {
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
    pub fn with_buffer_sizes(mut self, read_size: usize, write_size: usize) -> Self {
        self.read_buffer_size = read_size;
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
    pub fn with_router_mandatory(mut self, enabled: bool) -> Self {
        self.router_mandatory = enabled;
        self
    }

    /// Enable ROUTER handover mode.
    pub fn with_router_handover(mut self, enabled: bool) -> Self {
        self.router_handover = enabled;
        self
    }

    /// Enable ROUTER probe on connect.
    pub fn with_probe_router(mut self, enabled: bool) -> Self {
        self.probe_router = enabled;
        self
    }

    /// Enable XPUB verbose mode.
    pub fn with_xpub_verbose(mut self, enabled: bool) -> Self {
        self.xpub_verbose = enabled;
        self
    }

    /// Enable XPUB manual mode.
    pub fn with_xpub_manual(mut self, enabled: bool) -> Self {
        self.xpub_manual = enabled;
        self
    }

    /// Set XPUB welcome message.
    pub fn with_xpub_welcome_msg(mut self, msg: bytes::Bytes) -> Self {
        self.xpub_welcome_msg = Some(msg);
        self
    }

    /// Enable XSUB verbose unsubscribe.
    pub fn with_xsub_verbose_unsubs(mut self, enabled: bool) -> Self {
        self.xsub_verbose_unsubs = enabled;
        self
    }

    /// Enable message conflation (keep only last message).
    pub fn with_conflate(mut self, enabled: bool) -> Self {
        self.conflate = enabled;
        self
    }

    /// Set TCP keepalive mode.
    ///
    /// # Arguments
    ///
    /// * `mode` - `-1` for OS default, `0` to disable, `1` to enable
    pub fn with_tcp_keepalive(mut self, mode: i32) -> Self {
        self.tcp_keepalive = mode;
        self
    }

    /// Set TCP keepalive count (number of probes before timeout).
    ///
    /// # Arguments
    ///
    /// * `count` - `-1` for OS default, `> 0` for specific count
    pub fn with_tcp_keepalive_cnt(mut self, count: i32) -> Self {
        self.tcp_keepalive_cnt = count;
        self
    }

    /// Set TCP keepalive idle time (seconds before first probe).
    ///
    /// # Arguments
    ///
    /// * `seconds` - `-1` for OS default, `> 0` for specific idle time
    pub fn with_tcp_keepalive_idle(mut self, seconds: i32) -> Self {
        self.tcp_keepalive_idle = seconds;
        self
    }

    /// Set TCP keepalive interval (seconds between probes).
    ///
    /// # Arguments
    ///
    /// * `seconds` - `-1` for OS default, `> 0` for specific interval
    pub fn with_tcp_keepalive_intvl(mut self, seconds: i32) -> Self {
        self.tcp_keepalive_intvl = seconds;
        self
    }

    /// Enable REQ correlation mode (match replies to requests).
    pub fn with_req_correlate(mut self, enabled: bool) -> Self {
        self.req_correlate = enabled;
        self
    }

    /// Enable REQ relaxed mode (allow multiple outstanding requests).
    pub fn with_req_relaxed(mut self, enabled: bool) -> Self {
        self.req_relaxed = enabled;
        self
    }

    /// Set multicast rate (ZMQ_RATE).
    pub fn with_rate(mut self, rate: i32) -> Self {
        self.rate = rate;
        self
    }

    /// Set multicast recovery interval (ZMQ_RECOVERY_IVL).
    pub fn with_recovery_ivl(mut self, interval: Duration) -> Self {
        self.recovery_ivl = interval;
        self
    }

    /// Set OS send buffer size (ZMQ_SNDBUF).
    pub fn with_sndbuf(mut self, size: i32) -> Self {
        self.sndbuf = size;
        self
    }

    /// Set OS receive buffer size (ZMQ_RCVBUF).
    pub fn with_rcvbuf(mut self, size: i32) -> Self {
        self.rcvbuf = size;
        self
    }

    /// Set multicast TTL/hops (ZMQ_MULTICAST_HOPS).
    pub fn with_multicast_hops(mut self, hops: i32) -> Self {
        self.multicast_hops = hops;
        self
    }

    /// Set IP Type of Service (ZMQ_TOS).
    pub fn with_tos(mut self, tos: i32) -> Self {
        self.tos = tos;
        self
    }

    /// Set multicast maximum TPU (ZMQ_MULTICAST_MAXTPDU).
    pub fn with_multicast_maxtpdu(mut self, mtu: i32) -> Self {
        self.multicast_maxtpdu = mtu;
        self
    }

    /// Enable IPv6 support (ZMQ_IPV6).
    pub fn with_ipv6(mut self, enabled: bool) -> Self {
        self.ipv6 = enabled;
        self
    }

    /// Bind to specific device (ZMQ_BINDTODEVICE) - Linux only.
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
    pub fn with_plain_server(mut self, enabled: bool) -> Self {
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
    pub fn with_plain_credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
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
    pub fn with_curve_server(mut self, enabled: bool) -> Self {
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
    pub fn with_curve_keypair(mut self, publickey: [u8; 32], secretkey: [u8; 32]) -> Self {
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
    pub fn with_curve_serverkey(mut self, serverkey: [u8; 32]) -> Self {
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

    /// Add a subscription filter for SUB/XSUB sockets (ZMQ_SUBSCRIBE).
    ///
    /// SUB sockets MUST subscribe to at least one topic to receive messages.
    /// An empty filter (b"" or Bytes::new()) subscribes to all messages.
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

    /// Add an unsubscription filter for SUB/XSUB sockets (ZMQ_UNSUBSCRIBE).
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
    pub fn is_recv_nonblocking(&self) -> bool {
        matches!(self.recv_timeout, Some(d) if d.is_zero())
    }

    /// Check if send operation should be non-blocking.
    pub fn is_send_nonblocking(&self) -> bool {
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
        let opts = SocketOptions::new()
            .with_connect_routing_id(bytes::Bytes::from_static(b"peer-123"));

        assert_eq!(
            opts.connect_routing_id,
            Some(bytes::Bytes::from_static(b"peer-123"))
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
            .with_subscribe(bytes::Bytes::new())  // Subscribe to all
            .with_subscribe(bytes::Bytes::from("weather."))
            .with_subscribe(bytes::Bytes::from("stocks."));

        assert_eq!(opts.subscriptions.len(), 3);
        assert_eq!(opts.subscriptions[0], bytes::Bytes::new());
        assert_eq!(opts.subscriptions[1], bytes::Bytes::from("weather."));
        assert_eq!(opts.subscriptions[2], bytes::Bytes::from("stocks."));

        // Test with_subscriptions
        let opts2 = SocketOptions::new()
            .with_subscriptions(vec![
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
