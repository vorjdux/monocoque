//! Socket configuration options
//!
//! This module provides configuration options for ZeroMQ sockets, similar to
//! libzmq's socket options (zmq_setsockopt/zmq_getsockopt).

use std::time::Duration;

/// Socket configuration options.
///
/// These options control socket behavior including timeouts, buffer sizes,
/// and reliability features.
///
/// # Examples
///
/// ```
/// use monocoque_core::options::SocketOptions;
/// use std::time::Duration;
///
/// let opts = SocketOptions::default()
///     .with_recv_timeout(Duration::from_secs(5))
///     .with_send_timeout(Duration::from_secs(5));
/// ```
#[derive(Debug, Clone)]
pub struct SocketOptions {
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
        }
    }
}

impl SocketOptions {
    /// Create new socket options with default values.
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

    /// Check if receive operation should be non-blocking.
    pub fn is_recv_nonblocking(&self) -> bool {
        matches!(self.recv_timeout, Some(d) if d.is_zero())
    }

    /// Check if send operation should be non-blocking.
    pub fn is_send_nonblocking(&self) -> bool {
        matches!(self.send_timeout, Some(d) if d.is_zero())
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
}
