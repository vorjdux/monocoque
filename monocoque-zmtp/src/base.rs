//! Base socket infrastructure shared by all ZMQ socket types.
//!
//! This module provides `SocketBase<S>` which contains all common fields and
//! low-level I/O operations used by DEALER, ROUTER, REQ, REP, PUB, SUB sockets.
//!
//! # Design Philosophy
//!
//! - **Zero-cost abstraction**: Composition-based, no vtables or dynamic dispatch
//! - **Single source of truth**: Common logic implemented once
//! - **Type safety**: Generic over stream type `S`
//! - **Protocol safety**: PoisonGuard integration for cancellation safety
//! - **Reconnection support**: Optional endpoint storage and backoff logic

use bytes::{BufMut, Bytes, BytesMut};
use compio_io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use monocoque_core::buffer::SegmentedBuffer;
use monocoque_core::endpoint::Endpoint;
use monocoque_core::io::take_read_buffer;
use monocoque_core::options::SocketOptions;
use monocoque_core::poison::PoisonGuard;
use monocoque_core::reconnect::ReconnectState;
use monocoque_core::rt::TcpStream;
use std::fmt;
use std::io;
use std::time::Instant;
use tracing::{debug, trace, warn};

use crate::codec::ZmtpDecoder;
use crate::handshake::perform_handshake_with_options;
use crate::session::SocketType;

// ─────────────────────────────────────────────────────────────────────────────
// ZMTP heartbeat helpers (RFC 23 / ZMTP 3.1)
// ─────────────────────────────────────────────────────────────────────────────

/// ZMTP PING command name (1-byte length prefix + "PING").
const PING_CMD: &[u8] = b"\x04PING";
/// ZMTP PONG command name (1-byte length prefix + "PONG").
const PONG_CMD: &[u8] = b"\x04PONG";

/// Build a ZMTP PING command frame.
///
/// Wire format (ZMTP command):
/// - `0x04` flag byte (COMMAND, short frame)
/// - 1-byte body length
/// - Body: `\x04PING` followed by 2-byte big-endian TTL in tenths of a second
///
/// The TTL tells the peer how long (in tenths of a second) it should wait
/// before considering the connection dead if it receives no traffic from us.
pub fn build_ping_frame(ttl_tenths: u16) -> Bytes {
    let mut body = BytesMut::with_capacity(PING_CMD.len() + 2);
    body.extend_from_slice(PING_CMD);
    body.put_u16(ttl_tenths);
    let body = body.freeze();
    crate::utils::encode_frame(crate::utils::FLAG_COMMAND, &body)
}

/// Build a ZMTP PONG command frame (reply to a received PING).
///
/// Wire format:
/// - `0x04` flag byte (COMMAND, short frame)
/// - 1-byte body length
/// - Body: `\x04PONG`
pub fn build_pong_frame() -> Bytes {
    let body = Bytes::from_static(PONG_CMD);
    crate::utils::encode_frame(crate::utils::FLAG_COMMAND, &body)
}

/// Return `true` if the decoded command payload begins with the PING name.
pub fn is_ping_payload(payload: &[u8]) -> bool {
    payload.starts_with(PING_CMD) && payload.len().saturating_sub(PING_CMD.len()) <= 18
}

/// Return `true` if the decoded command payload begins with the PONG name.
pub fn is_pong_payload(payload: &[u8]) -> bool {
    payload.starts_with(PONG_CMD) && payload.len().saturating_sub(PONG_CMD.len()) <= 16
}

/// Base socket infrastructure shared by all ZMQ socket types.
///
/// Contains all common fields and low-level I/O operations. Each socket type
/// (DEALER, ROUTER, REQ, REP, etc.) composes this struct and adds socket-specific
/// logic on top.
///
/// # Fields
///
/// - **Connection state**: `stream`, `endpoint`, `reconnect`
/// - **Buffers**: `recv`, `send_buffer`, `write_buf`, `read_buf`
/// - **Protocol**: `decoder`, `is_poisoned`
/// - **Configuration**: `config`, `options`
///
/// # Zero-Cost Abstraction
///
/// This is a plain struct with no vtable. The compiler can inline all methods,
/// resulting in zero runtime overhead compared to duplicating code in each socket.
pub struct SocketBase<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Underlying stream (TCP or Unix socket) - None when disconnected
    pub(crate) stream: Option<S>,

    /// Optional endpoint for automatic reconnection
    pub(crate) endpoint: Option<Endpoint>,

    /// Reconnection state tracker (exponential backoff)
    pub(crate) reconnect: Option<ReconnectState>,

    /// ZMTP frame decoder
    pub(crate) decoder: ZmtpDecoder,

    /// Segmented read buffer for incoming data
    pub(crate) recv: SegmentedBuffer,

    /// Reusable read slab; `take_read_buffer` carves each read off its tail.
    pub(crate) read_buf: BytesMut,

    /// Reusable write buffer for outgoing data
    pub(crate) write_buf: BytesMut,

    /// Reusable iovec scratch for vectored writes (header/body entries),
    /// kept across calls so `send_vectored` allocates nothing on the hot path.
    pub(crate) iov: Vec<Bytes>,

    /// Send buffer for message batching
    pub(crate) send_buffer: BytesMut,

    /// Socket options (timeouts, limits, identity, buffer sizes)
    pub(crate) options: SocketOptions,

    /// Last connected/bound endpoint
    pub(crate) last_endpoint: Option<String>,

    /// Connection health flag (true if I/O was cancelled mid-operation)
    pub(crate) is_poisoned: bool,

    /// Number of messages currently buffered (for HWM enforcement)
    pub(crate) buffered_messages: usize,

    // ── Heartbeat state (ZMTP PING/PONG, RFC 23) ──────────────────────────
    //
    // Heartbeating keeps idle connections alive and detects dead peers.
    // When `options.heartbeat_ivl` is Some(dur):
    //   1. `last_recv_instant` is updated on every received frame.
    //   2. If `heartbeat_ivl` elapses with no received data, a PING command
    //      is sent and `ping_sent_at` records the transmission time.
    //   3. The peer must reply with a PONG within `heartbeat_timeout`
    //      (defaults to `heartbeat_ivl` when not set).  If it does not,
    //      `awaiting_pong` stays true and the next check can close the conn.
    //
    // Stub note: PING sending is wired into `check_heartbeat()`.
    // Timeout-based disconnection is noted below and left for a future PR.
    /// Instant of the last received frame on this connection.
    ///
    /// `None` before the first frame is received after the handshake, or
    /// when heartbeating is disabled.
    pub(crate) last_recv_instant: Option<Instant>,

    /// Instant at which the most recent PING was sent.
    ///
    /// `None` when no PING is outstanding.
    pub(crate) ping_sent_at: Option<Instant>,

    /// Whether we are waiting for a PONG reply to a sent PING.
    ///
    /// Set to `true` when a PING is transmitted; cleared when a matching
    /// PONG command frame is received.
    pub(crate) awaiting_pong: bool,

    /// Post-handshake CURVE cipher, if CURVE security is active.
    pub(crate) curve_cipher: Option<crate::security::curve::CurveMessageCipher>,
}

pub fn append_zmtp_cmd_frame(buf: &mut BytesMut, body: &[u8]) {
    let len = body.len();
    if len <= 255 {
        buf.extend_from_slice(&[0x04, len as u8]);
    } else {
        buf.extend_from_slice(&[0x06]);
        buf.extend_from_slice(&(len as u64).to_be_bytes());
    }
    buf.extend_from_slice(body);
}

/// Result of processing one decoded ZMTP frame.
pub enum FrameResult {
    /// A data frame or decrypted CURVE MESSAGE: (more_flag, payload)
    Data(bool, Bytes),
    /// A command frame was handled internally (PING replied, PONG noted).
    /// The send_buffer may have been written; call flush_send_buffer if needed.
    CommandHandled,
    /// Need more data from the wire.
    NeedMore,
}

/// Apply equal jitter to a reconnect backoff delay, spreading the actual sleep
/// uniformly across `[delay/2, delay]`.
///
/// Without jitter, a fleet of clients that lost a restarted server would all
/// wake from the same backoff in lockstep and hammer it in synchronized waves.
/// Jitter decorrelates them while preserving the backoff's growth.
fn jittered_backoff(delay: std::time::Duration) -> std::time::Duration {
    use rand::Rng;
    if delay.is_zero() {
        return delay;
    }
    // Backoff delays are milliseconds to seconds, so the nanosecond count fits
    // comfortably in u64.
    let half_ns = (delay.as_nanos() / 2) as u64;
    std::time::Duration::from_nanos(half_ns + rand::thread_rng().gen_range(0..=half_ns))
}

impl<S> SocketBase<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new SocketBase with the given stream and options.
    ///
    /// This is used when the socket is created from an existing stream
    /// (e.g., `from_tcp`, `from_unix_stream`). No endpoint or reconnection
    /// state is stored.
    ///
    /// Buffer sizes are taken from `options.read_buffer_size` and `options.write_buffer_size`.
    pub fn new(stream: S, _socket_type: SocketType, options: SocketOptions) -> Self {
        let write_capacity = options.write_buffer_size;
        let decoder = if let Some(max) = options.max_msg_size {
            ZmtpDecoder::with_max_frame_size(max)
        } else {
            ZmtpDecoder::new()
        };
        Self {
            stream: Some(stream),
            endpoint: None,
            reconnect: None,
            decoder,
            recv: SegmentedBuffer::new(),
            // Lazily allocated on the first read (matches the old arena, which
            // allocated no page until first use), so an idle socket holds none.
            read_buf: BytesMut::new(),
            write_buf: BytesMut::with_capacity(write_capacity),
            iov: Vec::new(),
            send_buffer: BytesMut::with_capacity(write_capacity),
            options,
            last_endpoint: None,
            is_poisoned: false,
            buffered_messages: 0,
            // Heartbeat fields  -  initialised to idle state
            last_recv_instant: None,
            ping_sent_at: None,
            awaiting_pong: false,
            curve_cipher: None,
        }
    }

    /// Create a new SocketBase with endpoint storage for reconnection.
    ///
    /// This is used when the socket is created via `connect(endpoint)` and
    /// automatic reconnection is desired.
    ///
    /// Buffer sizes are taken from `options.read_buffer_size` and `options.write_buffer_size`.
    pub fn with_endpoint(
        stream: S,
        _socket_type: SocketType,
        endpoint: Endpoint,
        options: SocketOptions,
    ) -> Self {
        let endpoint_str = endpoint.to_string();
        let write_capacity = options.write_buffer_size;
        let decoder = if let Some(max) = options.max_msg_size {
            ZmtpDecoder::with_max_frame_size(max)
        } else {
            ZmtpDecoder::new()
        };
        Self {
            stream: Some(stream),
            endpoint: Some(endpoint),
            reconnect: Some(ReconnectState::new(&options)),
            decoder,
            recv: SegmentedBuffer::new(),
            // Lazily allocated on the first read (matches the old arena, which
            // allocated no page until first use), so an idle socket holds none.
            read_buf: BytesMut::new(),
            write_buf: BytesMut::with_capacity(write_capacity),
            iov: Vec::new(),
            send_buffer: BytesMut::with_capacity(write_capacity),
            options,
            last_endpoint: Some(endpoint_str),
            is_poisoned: false,
            buffered_messages: 0,
            // Heartbeat fields  -  initialised to idle state
            last_recv_instant: None,
            ping_sent_at: None,
            awaiting_pong: false,
            curve_cipher: None,
        }
    }

    /// Check if the socket is connected.
    #[inline]
    pub const fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Check if the socket is poisoned (I/O was cancelled mid-operation).
    #[inline]
    pub const fn is_poisoned(&self) -> bool {
        self.is_poisoned
    }

    /// Get the number of buffered messages.
    #[inline]
    pub const fn buffered_messages(&self) -> usize {
        self.buffered_messages
    }

    /// Get the number of buffered bytes.
    #[inline]
    pub fn buffered_bytes(&self) -> usize {
        self.send_buffer.len()
    }

    /// Update live socket options and keep derived decoder state in sync.
    pub(crate) fn set_options(&mut self, options: SocketOptions) {
        self.decoder.set_max_body_len(options.max_msg_size);
        self.options = options;
    }

    /// Check if send HWM has been reached.
    #[inline]
    pub const fn hwm_reached(&self) -> bool {
        self.options.send_hwm != 0 && self.buffered_messages >= self.options.send_hwm
    }

    /// Get the endpoint this socket is connected/bound to, if any.
    ///
    /// Returns `None` if the socket was created from a raw stream without
    /// endpoint information.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub const fn last_endpoint(&self) -> Option<&Endpoint> {
        self.endpoint.as_ref()
    }

    /// Get the last endpoint as a string.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_LAST_ENDPOINT` (32) option.
    #[inline]
    pub fn last_endpoint_string(&self) -> Option<&str> {
        self.last_endpoint.as_deref()
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    // socket_type field is used internally for reconnection logic
    // Each socket implementation provides its own public socket_type() method
    /// Check if more message frames are expected (for multipart messages).
    ///
    /// This indicates whether the last received message has more frames
    /// coming after it. Always returns `false` for single-frame messages.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_RCVMORE` (13) option.
    pub fn has_more(&self) -> bool {
        self.decoder.has_more()
    }

    /// Get current socket events (read/write readiness).
    ///
    /// Returns a bitmask indicating which operations can proceed without blocking:
    /// - `POLLIN` (1): Socket has messages ready to read
    /// - `POLLOUT` (2): Socket can accept messages for sending
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_EVENTS` (15) option.
    ///
    /// # Note
    ///
    /// This is a best-effort check based on current buffer state.
    /// For true async readiness, use the async recv/send operations.
    #[inline]
    pub fn events(&self) -> u32 {
        let mut events = 0u32;

        // POLLIN (1): Can receive if connected and buffers available
        if self.is_connected() && !self.is_poisoned {
            events |= 1; // POLLIN
        }

        // POLLOUT (2): Can send if connected and HWM not reached
        if self.is_connected() && !self.hwm_reached() && !self.is_poisoned {
            events |= 2; // POLLOUT
        }

        events
    }

    // ── Heartbeat helpers ────────────────────────────────────────────────────

    /// Record that a frame was received right now.
    ///
    /// Call this every time a complete ZMTP frame is read from the wire so that
    /// the heartbeat idle timer is reset.  When heartbeating is disabled
    /// (`options.heartbeat_ivl` is `None`) this is a no-op.
    #[inline]
    pub fn note_recv(&mut self) {
        if self.options.heartbeat_ivl.is_some() {
            self.last_recv_instant = Some(Instant::now());
        }
    }

    /// Record that a PONG was received, clearing the outstanding-PING flag.
    ///
    /// Call this when a command frame whose payload starts with `\x04PONG`
    /// is decoded in the active phase.
    #[inline]
    pub fn note_pong_received(&mut self) {
        if self.awaiting_pong {
            trace!("[SocketBase] PONG received  -  heartbeat round-trip complete");
            self.awaiting_pong = false;
            self.ping_sent_at = None;
        }
    }

    /// Check whether a PING should be sent and whether a pending PONG has
    /// timed out.
    ///
    /// This method should be called periodically in the socket's receive loop
    /// (e.g., after every successful `read_raw`).  It encodes the PING frame
    /// directly into `send_buffer` so the caller can flush it.
    ///
    /// # Return value
    ///
    /// - `Ok(true)`   -  a PING was appended to `send_buffer`; caller must flush.
    /// - `Ok(false)`  -  nothing to do or heartbeat is disabled.
    /// - `Err(e)`     -  a pending PONG timed out; connection should be closed.
    ///   Callers propagate this with `?` so the error surfaces to the application.
    pub fn check_heartbeat(&mut self) -> io::Result<bool> {
        let Some(ivl) = self.options.heartbeat_ivl else {
            return Ok(false);
        };

        let now = Instant::now();

        // ── Check PONG timeout ───────────────────────────────────────────
        if self.awaiting_pong {
            if let Some(ping_at) = self.ping_sent_at {
                let timeout = self.options.heartbeat_timeout.unwrap_or(ivl); // default to ivl when not set
                if now.duration_since(ping_at) > timeout {
                    warn!(
                        "[SocketBase] Heartbeat PONG not received within {:?}  -  peer considered dead",
                        timeout
                    );
                    // Mark disconnected
                    self.stream = None;
                    self.awaiting_pong = false;
                    self.ping_sent_at = None;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "ZMTP heartbeat: no PONG received within timeout",
                    ));
                }
            }
            // Still waiting for PONG  -  don't send another PING
            return Ok(false);
        }

        // ── Check whether we should send a PING ─────────────────────────
        let idle_since = self.last_recv_instant.unwrap_or_else(|| {
            // No frame received yet  -  treat as idle from the start
            now.checked_sub(ivl + ivl).unwrap_or(now)
        });

        if now.duration_since(idle_since) >= ivl {
            // Compute TTL to advertise: use heartbeat_ttl if set, else ivl
            let ttl_dur = self.options.heartbeat_ttl.unwrap_or(ivl);
            // ZMTP TTL is in tenths of a second, capped at u16::MAX
            let ttl_tenths = (ttl_dur.as_millis() / 100).min(u128::from(u16::MAX)) as u16;

            let ping = build_ping_frame(ttl_tenths);
            self.send_buffer.extend_from_slice(&ping);
            self.ping_sent_at = Some(now);
            self.awaiting_pong = true;

            debug!(
                "[SocketBase] Sending PING (ttl_tenths={}, idle={:?})",
                ttl_tenths,
                now.duration_since(idle_since)
            );
            return Ok(true);
        }

        Ok(false)
    }

    /// Read raw bytes from the stream into the recv buffer without decoding.
    ///
    /// This is the low-level read primitive used by socket implementations to
    /// accumulate multipart messages. Callers should manually decode frames
    /// from the recv buffer using `decoder.decode()`.
    ///
    /// Returns:
    /// - `Ok(n)` where n is the number of bytes read (n > 0)
    /// - `Ok(0)` if EOF was reached (connection closed)
    /// - `Err(e)` on I/O error
    ///
    /// On EOF, sets `stream = None` to mark disconnection.
    pub(crate) async fn read_raw(&mut self) -> io::Result<usize> {
        // Ensure we're connected
        if self.stream.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Socket not connected",
            ));
        }

        // Read from stream
        use compio_buf::BufResult;

        // Reject the non-blocking / zero-timeout case before taking a buffer.
        if matches!(self.options.recv_timeout, Some(dur) if dur.is_zero()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Socket is in non-blocking mode and no data is available",
            ));
        }

        // SAFETY: `buf` is passed straight to `read` below; on every path that
        // exposes bytes it is first truncated to `n`, and the error/EOF paths
        // drop it without inspecting its contents.
        let buf = unsafe { take_read_buffer(&mut self.read_buf, self.options.read_buffer_size()) };

        // Get stream reference only for I/O
        let stream = self
            .stream
            .as_mut()
            .expect("BUG: stream must be Some  -  checked is_none() above");

        // Apply recv timeout
        let BufResult(result, mut buf) = match self.options.recv_timeout {
            None => AsyncRead::read(stream, buf).await,
            Some(dur) => {
                use monocoque_core::rt::timeout;
                match timeout(dur, AsyncRead::read(stream, buf)).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("Receive operation timed out after {:?}", dur),
                        ));
                    }
                }
            }
        };

        let n = result?;

        if n == 0 {
            // EOF - mark stream as disconnected
            trace!("[SocketBase] Connection closed (EOF)");
            self.stream = None;
            return Ok(0);
        }

        // Push bytes into recv buffer (trim to what was actually read).
        // Small frames are copied out of the shared 64 KiB read slab so a
        // single lagging frame does not pin the whole slab by refcount (see
        // io::take_read_buffer / COPY_OUT_THRESHOLD). Larger frames stay
        // zero-copy via freeze().
        buf.truncate(n);
        if n < monocoque_core::io::COPY_OUT_THRESHOLD {
            self.recv.push(Bytes::copy_from_slice(&buf));
        } else {
            self.recv.push(buf.freeze());
        }

        // Update heartbeat idle timer: data was received so we are not idle
        self.note_recv();

        Ok(n)
    }

    /// Write buffered data from `send_buffer` to the stream.
    ///
    /// Uses PoisonGuard to ensure cancellation safety. If this method is
    /// cancelled during the write, the socket will be marked poisoned.
    ///
    /// Returns `Ok(())` on success, `Err(e)` on failure. On write failure,
    /// sets `stream = None` to mark disconnection.
    pub(crate) async fn flush_send_buffer(&mut self) -> io::Result<()> {
        if self.send_buffer.is_empty() {
            return Ok(());
        }

        // Check health before attempting I/O
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket poisoned by cancelled I/O - reconnect required",
            ));
        }

        // Ensure we have a connected stream
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Socket not connected"))?;

        if self.options.send_timeout.is_some_and(|dur| dur.is_zero()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Socket is in non-blocking mode and cannot flush immediately",
            ));
        }

        trace!("[SocketBase] Flushing {} bytes", self.send_buffer.len());

        // Arm poison guard
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        use compio_buf::BufResult;
        let buf = self.send_buffer.split().freeze();

        // Apply send timeout
        let BufResult(result, _) = match self.options.send_timeout {
            None => stream.write_all(buf).await,
            Some(dur) => {
                use monocoque_core::rt::timeout;
                match timeout(dur, stream.write_all(buf)).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("Flush operation timed out after {:?}", dur),
                        ));
                    }
                }
            }
        };

        let write_result = result;

        // If write failed, mark stream as disconnected
        if write_result.is_err() {
            self.stream = None;
        }

        write_result?;

        // Success - disarm guard and reset counter
        guard.disarm();
        self.buffered_messages = 0;

        trace!("[SocketBase] Flush completed");
        Ok(())
    }

    /// Write the contents of write_buf directly to the stream.
    ///
    /// This is used when the caller has already encoded data into write_buf
    /// and wants to send it without additional copying. Applies send_timeout
    /// from options and uses PoisonGuard for cancellation safety.
    pub(crate) async fn write_from_buf(&mut self) -> io::Result<()> {
        // Check health
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket poisoned by cancelled I/O",
            ));
        }

        // Ensure we have a connected stream
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Socket not connected"))?;

        if self.options.send_timeout.is_some_and(|dur| dur.is_zero()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Socket is in non-blocking mode and cannot send immediately",
            ));
        }

        // Arm poison guard
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        // Send write_buf contents
        let buf = self.write_buf.split().freeze();

        use compio_buf::BufResult;

        // Apply send timeout from options
        let BufResult(result, _) = match self.options.send_timeout {
            None => {
                // Blocking mode - no timeout
                stream.write_all(buf).await
            }
            Some(dur) => {
                // Timed mode - apply timeout
                use monocoque_core::rt::timeout;
                match timeout(dur, stream.write_all(buf)).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("Send operation timed out after {:?}", dur),
                        ));
                    }
                }
            }
        };

        // Mark disconnected on error
        if result.is_err() {
            self.stream = None;
        }

        result?;

        guard.disarm();
        Ok(())
    }

    /// Return `true` if `msg` should be sent with a vectored write rather than
    /// the copy-into-`send_buffer` path.
    ///
    /// Vectored writes pay off once a frame body is large enough that copying it
    /// into the userspace send buffer dominates the per-message cost. Small
    /// frames stay on the copy path, where a single contiguous `write` beats the
    /// per-iovec bookkeeping. CURVE-encrypted connections never qualify: the
    /// cipher must transform each body into a fresh buffer regardless, so there
    /// is no copy to save.
    #[inline]
    pub(crate) fn should_vectored_write(&self, msg: &[Bytes]) -> bool {
        if self.curve_cipher.is_some() {
            return false;
        }
        let threshold = self.options.vectored_write_threshold;
        msg.iter().any(|frame| frame.len() >= threshold)
    }

    /// Send a multipart message using a vectored write, without copying any
    /// frame body into the userspace send buffer.
    ///
    /// Each frame contributes two iovec entries: a freshly built header (2 or 9
    /// bytes) and the frame body itself, an O(1) `Bytes::clone` with no data
    /// copy. The whole iovec list is handed to `write_vectored_all`, so the
    /// bodies travel straight to the kernel.
    ///
    /// Anything already pending in `send_buffer` is flushed first to preserve
    /// wire ordering. Uses `PoisonGuard` for cancellation safety and applies
    /// `send_timeout`, mirroring [`flush_send_buffer`](Self::flush_send_buffer).
    /// On write failure the stream is dropped (`stream = None`) to mark
    /// disconnection.
    pub(crate) async fn send_vectored(&mut self, msg: &[Bytes]) -> io::Result<()> {
        use crate::codec::write_frame_header;

        if msg.is_empty() {
            return Ok(());
        }

        // Preserve ordering: flush anything already buffered before this frame.
        if !self.send_buffer.is_empty() {
            self.flush_send_buffer().await?;
        }

        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket poisoned by cancelled I/O - reconnect required",
            ));
        }

        if self.options.send_timeout.is_some_and(|dur| dur.is_zero()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Socket is in non-blocking mode and cannot send immediately",
            ));
        }

        // Build all frame headers contiguously in the reused write_buf, then
        // slice each one back out (O(1), sharing write_buf's allocation). The
        // iovec list is reused across calls via `self.iov`, so the hot path
        // performs no per-message heap allocation.
        let last = msg.len() - 1;
        self.write_buf.clear();
        for (i, frame) in msg.iter().enumerate() {
            write_frame_header(&mut self.write_buf, frame.len(), i < last);
        }
        let mut headers = self.write_buf.split().freeze();

        let mut iovecs = std::mem::take(&mut self.iov);
        iovecs.clear();
        iovecs.reserve(msg.len() * 2);
        for frame in msg {
            let hlen = if frame.len() >= 256 { 9 } else { 2 };
            iovecs.push(headers.split_to(hlen));
            iovecs.push(frame.clone());
        }

        let Some(stream) = self.stream.as_mut() else {
            self.iov = iovecs; // keep the scratch capacity
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Socket not connected",
            ));
        };

        // Arm poison guard for cancellation safety.
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        use compio_buf::BufResult;
        let BufResult(result, returned) = match self.options.send_timeout {
            None => stream.write_vectored_all(iovecs).await,
            Some(dur) => {
                use monocoque_core::rt::timeout;
                match timeout(dur, stream.write_vectored_all(iovecs)).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("Send operation timed out after {:?}", dur),
                        ));
                    }
                }
            }
        };

        // Reclaim the iovec allocation for the next call.
        self.iov = returned;

        if result.is_err() {
            self.stream = None;
        }
        result?;

        guard.disarm();
        Ok(())
    }

    /// Close the socket gracefully, honoring LINGER for any buffered send data.
    ///
    /// Coalesced-but-unflushed data in `send_buffer` is drained according to the
    /// `linger` option before the stream is shut down:
    /// - `Some(0)`: discard buffered data immediately.
    /// - `Some(dur)`: flush within `dur`, then close even if the flush did not
    ///   complete in time.
    /// - `None`: flush indefinitely until all buffered data is sent.
    ///
    /// Then sends a TCP FIN (or equivalent) and drops the connection. After this
    /// call `is_connected()` returns `false`.
    pub async fn close(&mut self) -> io::Result<()> {
        // Drain any coalesced-but-unflushed data per LINGER before shutdown, so
        // callers relying on close() to flush do not silently lose the tail of a
        // coalesced burst.
        if !self.send_buffer.is_empty() && self.stream.is_some() {
            match self.options.linger {
                Some(dur) if dur.is_zero() => {
                    // Linger 0: discard buffered data.
                    self.send_buffer.clear();
                    self.buffered_messages = 0;
                }
                Some(dur) => {
                    use monocoque_core::rt::timeout;
                    // Flush within the linger window; close anyway on timeout.
                    match timeout(dur, self.flush_send_buffer()).await {
                        Ok(Ok(())) | Err(_) => {}
                        Ok(Err(e)) => return Err(e),
                    }
                }
                None => {
                    // Linger indefinite: block until flushed.
                    self.flush_send_buffer().await?;
                }
            }
        }

        if let Some(ref mut stream) = self.stream {
            stream.shutdown().await?;
        }
        self.stream = None;
        Ok(())
    }

    /// Encode a multipart message into `write_buf`, encrypting if CURVE is active.
    pub fn encode_message_to_write_buf(&mut self, msg: &[Bytes]) -> io::Result<()> {
        use crate::codec::encode_multipart;
        self.write_buf.clear();
        if let Some(ref mut cipher) = self.curve_cipher {
            let last = msg.len().saturating_sub(1);
            for (i, frame) in msg.iter().enumerate() {
                let body = cipher
                    .encrypt_frame(frame, i < last)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                append_zmtp_cmd_frame(&mut self.write_buf, &body);
            }
        } else {
            encode_multipart(msg, &mut self.write_buf);
        }
        Ok(())
    }

    /// Encode a multipart message into `send_buffer`, encrypting if CURVE is active.
    pub fn encode_message_to_send_buf(&mut self, msg: &[Bytes]) -> io::Result<()> {
        use crate::codec::encode_multipart;
        if let Some(ref mut cipher) = self.curve_cipher {
            let last = msg.len().saturating_sub(1);
            for (i, frame) in msg.iter().enumerate() {
                let body = cipher
                    .encrypt_frame(frame, i < last)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                append_zmtp_cmd_frame(&mut self.send_buffer, &body);
            }
        } else {
            encode_multipart(msg, &mut self.send_buffer);
        }
        self.buffered_messages += 1;
        Ok(())
    }

    /// Encode `msg` into `send_buffer` and flush when the coalesce threshold is reached.
    ///
    /// This is the hot path used when `SocketOptions::write_coalescing` is enabled.
    /// It does **not** touch `buffered_messages` (which is reserved for the explicit
    /// `send_buffered` / `flush` batch API).  Callers must call `flush_send_buffer`
    /// after the last message in a burst.
    pub(crate) async fn send_coalesced(&mut self, msg: &[Bytes]) -> io::Result<()> {
        use crate::codec::encode_multipart;
        if let Some(ref mut cipher) = self.curve_cipher {
            let last = msg.len().saturating_sub(1);
            for (i, frame) in msg.iter().enumerate() {
                let body = cipher
                    .encrypt_frame(frame, i < last)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                append_zmtp_cmd_frame(&mut self.send_buffer, &body);
            }
        } else {
            encode_multipart(msg, &mut self.send_buffer);
        }
        if self.send_buffer.len() >= self.options.write_coalesce_threshold {
            self.flush_send_buffer().await?;
        }
        Ok(())
    }

    /// Encode one data frame into `send_buffer`.
    ///
    /// Returns `true` when the coalescing threshold has been reached and the
    /// caller should flush.
    pub(crate) fn encode_one_coalesced(&mut self, frame: &Bytes) -> io::Result<bool> {
        if self.curve_cipher.is_none() {
            crate::codec::encode_single(frame, &mut self.send_buffer);
            return Ok(self.send_buffer.len() >= self.options.write_coalesce_threshold);
        }
        self.encode_one_curve_coalesced(frame)
    }

    fn encode_one_curve_coalesced(&mut self, frame: &Bytes) -> io::Result<bool> {
        let cipher = self
            .curve_cipher
            .as_mut()
            .expect("checked by encode_one_coalesced");
        let body = cipher
            .encrypt_frame(frame, false)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        append_zmtp_cmd_frame(&mut self.send_buffer, &body);
        Ok(self.send_buffer.len() >= self.options.write_coalesce_threshold)
    }

    /// Decode the next frame from the receive buffer, handling CURVE decryption and PING/PONG.
    pub fn process_frame(&mut self) -> io::Result<FrameResult> {
        use crate::security::curve::CurveMessageCipher;
        match self
            .decoder
            .decode(&mut self.recv)
            .map_err(io::Error::from)?
        {
            None => Ok(FrameResult::NeedMore),
            Some(frame) => {
                if frame.is_command() {
                    // CURVE MESSAGE decryption
                    if let Some(ref mut cipher) = self.curve_cipher
                        && CurveMessageCipher::is_curve_message(&frame.payload)
                    {
                        let (more, payload) =
                            cipher.decrypt_frame(&frame.payload).map_err(|e| {
                                io::Error::new(io::ErrorKind::InvalidData, e.to_string())
                            })?;
                        return Ok(FrameResult::Data(more, payload));
                    }
                    // PING/PONG
                    if is_ping_payload(&frame.payload) {
                        let pong = build_pong_frame();
                        self.send_buffer.extend_from_slice(&pong);
                    }
                    if is_pong_payload(&frame.payload) {
                        self.note_pong_received();
                    }
                    Ok(FrameResult::CommandHandled)
                } else {
                    // Reject raw data frames when CURVE is negotiated - all application
                    // messages must arrive as CURVE MESSAGE command frames.
                    if self.curve_cipher.is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected plaintext data frame in CURVE mode",
                        ));
                    }
                    Ok(FrameResult::Data(frame.more(), frame.payload))
                }
            }
        }
    }
}

impl SocketBase<TcpStream> {
    /// Try to reconnect to the stored endpoint.
    ///
    /// This method:
    /// 1. Checks if endpoint is configured
    /// 2. Applies exponential backoff delay
    /// 3. Attempts new TCP connection
    /// 4. Performs ZMTP handshake
    /// 5. Resets socket state on success
    ///
    /// Returns `Ok(())` on successful reconnection, `Err(e)` otherwise.
    pub(crate) async fn try_reconnect(&mut self, socket_type: SocketType) -> io::Result<()> {
        // Can only reconnect if we have an endpoint
        let endpoint = self.endpoint.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Socket was not created with connect() - no endpoint stored for reconnection",
            )
        })?;

        // Apply the backoff delay if we have reconnection state. This is an
        // async sleep that yields the executor, so a reconnecting socket does
        // not stall other sockets colocated on the same single-threaded runtime.
        // (Earlier this had to be a blocking std::thread::sleep because compio
        // 0.10 left residual timer state after handshake timeouts that hung
        // rt::sleep; compio 0.19 fixed that, verified by
        // tests/sleep_after_timeout_probe.rs.)
        if let Some(reconnect) = &mut self.reconnect {
            let base_delay = reconnect.next_delay();
            let delay = jittered_backoff(base_delay);
            debug!(
                "[SocketBase] Reconnection attempt {} after {:?}",
                reconnect.attempt(),
                delay
            );
            monocoque_core::rt::sleep(delay).await;
        }

        // Attempt connection based on endpoint type
        let mut new_stream = match endpoint {
            Endpoint::Tcp(addr) => TcpStream::connect(addr).await?,
            #[cfg(unix)]
            Endpoint::Ipc(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IPC reconnection not supported for TcpStream base",
                ));
            }
            Endpoint::Inproc(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Inproc reconnection not supported for TcpStream base",
                ));
            }
        };

        // Re-apply TCP tuning to the fresh socket. The original connect set
        // TCP_NODELAY (and keepalive), but a reconnect is a brand-new fd that
        // starts with kernel defaults (Nagle on), so without this the socket
        // would silently run with Nagle enabled after any reconnect. This is a
        // one-time setsockopt at reconnect, off the send/recv hot path.
        crate::utils::configure_tcp_stream(&new_stream, &self.options, "RECONNECT")?;

        // Perform handshake  -  preserve routing identity from options
        let hr = perform_handshake_with_options(
            &mut new_stream,
            socket_type,
            self.options.routing_id.as_deref(),
            Some(self.options.handshake_timeout),
            &self.options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed during reconnect: {}", e)))?;

        // Success! Update socket state
        self.curve_cipher = hr.curve_cipher;
        self.stream = Some(new_stream);
        self.is_poisoned = false;
        self.recv = SegmentedBuffer::new();
        self.send_buffer.clear();
        self.buffered_messages = 0;

        // Reset heartbeat state for the fresh connection
        self.last_recv_instant = None;
        self.ping_sent_at = None;
        self.awaiting_pong = false;

        // Reset reconnection state
        if let Some(ref mut reconnect) = self.reconnect {
            reconnect.reset();
        }

        debug!("[SocketBase] Reconnection successful");
        Ok(())
    }
}

impl<S> fmt::Debug for SocketBase<S>
where
    S: AsyncRead + AsyncWrite + Unpin + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketBase")
            .field("connected", &self.is_connected())
            .field("poisoned", &self.is_poisoned)
            .field("buffered_messages", &self.buffered_messages)
            .field("buffered_bytes", &self.buffered_bytes())
            .field("endpoint", &self.endpoint)
            .field("awaiting_pong", &self.awaiting_pong)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compio_buf::{BufResult, IoBuf, IoBufMut};
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};

    #[test]
    fn jittered_backoff_stays_within_half_to_full_range() {
        use std::time::Duration;
        // Zero stays zero (no reconnect delay).
        assert_eq!(jittered_backoff(Duration::ZERO), Duration::ZERO);

        // Otherwise the jittered delay is always in [delay/2, delay]. Sample
        // enough draws to exercise the range without relying on a fixed seed.
        let delay = Duration::from_millis(100);
        for _ in 0..1000 {
            let j = jittered_backoff(delay);
            assert!(
                j >= delay / 2 && j <= delay,
                "jittered delay {j:?} out of [{:?}, {:?}]",
                delay / 2,
                delay
            );
        }
    }

    const PAYLOAD: &[u8] = b"abcdef";

    #[derive(Clone, Debug, Default)]
    struct WriteLog(Arc<Mutex<Vec<Vec<u8>>>>);

    impl WriteLog {
        fn push(&self, bytes: &[u8]) {
            self.0.lock().unwrap().push(bytes.to_vec());
        }

        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().concat()
        }

        fn write_count(&self) -> usize {
            self.0.lock().unwrap().len()
        }

        fn is_empty(&self) -> bool {
            self.0.lock().unwrap().is_empty()
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum WriteStep {
        Bytes(usize),
        Error(io::ErrorKind),
    }

    #[derive(Debug)]
    struct ScriptedWriteStream {
        steps: VecDeque<WriteStep>,
        log: WriteLog,
    }

    impl ScriptedWriteStream {
        fn new(steps: impl IntoIterator<Item = WriteStep>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                log: WriteLog::default(),
            }
        }

        fn log(&self) -> WriteLog {
            self.log.clone()
        }
    }

    impl AsyncRead for ScriptedWriteStream {
        async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
            BufResult(Ok(0), buf)
        }
    }

    impl AsyncWrite for ScriptedWriteStream {
        async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
            match self.steps.pop_front() {
                Some(WriteStep::Bytes(n)) => {
                    let n = n.min(buf.buf_len());
                    self.log.push(&buf.as_init()[..n]);
                    BufResult(Ok(n), buf)
                }
                Some(WriteStep::Error(kind)) => {
                    BufResult(Err(io::Error::new(kind, "scripted write error")), buf)
                }
                None => {
                    let n = buf.buf_len();
                    self.log.push(buf.as_init());
                    BufResult(Ok(n), buf)
                }
            }
        }

        async fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum WritePath {
        WriteFromBuf,
        FlushSendBuffer,
    }

    impl WritePath {
        fn buffer_payload(self, base: &mut SocketBase<ScriptedWriteStream>) {
            match self {
                Self::WriteFromBuf => base.write_buf.extend_from_slice(PAYLOAD),
                Self::FlushSendBuffer => {
                    base.send_buffer.extend_from_slice(PAYLOAD);
                    base.buffered_messages = 1;
                }
            }
        }

        async fn write(self, base: &mut SocketBase<ScriptedWriteStream>) -> io::Result<()> {
            match self {
                Self::WriteFromBuf => base.write_from_buf().await,
                Self::FlushSendBuffer => base.flush_send_buffer().await,
            }
        }

        fn assert_drained(self, base: &SocketBase<ScriptedWriteStream>) {
            match self {
                Self::WriteFromBuf => assert!(base.write_buf.is_empty()),
                Self::FlushSendBuffer => {
                    assert!(base.send_buffer.is_empty());
                    assert_eq!(base.buffered_messages, 0);
                }
            }
        }

        fn assert_payload_buffered(self, base: &SocketBase<ScriptedWriteStream>) {
            match self {
                Self::WriteFromBuf => assert_eq!(&base.write_buf[..], PAYLOAD),
                Self::FlushSendBuffer => {
                    assert_eq!(&base.send_buffer[..], PAYLOAD);
                    assert_eq!(base.buffered_messages, 1);
                }
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum StreamState {
        Connected,
        Disconnected,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum WriteStatus {
        Ok,
        Err(io::ErrorKind),
    }

    impl WriteStatus {
        fn from_result(result: &io::Result<()>) -> Self {
            match result {
                Ok(()) => Self::Ok,
                Err(err) => Self::Err(err.kind()),
            }
        }
    }

    #[derive(Debug)]
    struct ExpectedWriteOutcome {
        status: WriteStatus,
        written: Vec<u8>,
    }

    fn simulate_write_all(payload: &[u8], script: &[WriteStep]) -> ExpectedWriteOutcome {
        let mut offset = 0;
        let mut written = Vec::new();

        for step in script {
            if offset == payload.len() {
                return ExpectedWriteOutcome {
                    status: WriteStatus::Ok,
                    written,
                };
            }

            match *step {
                WriteStep::Bytes(0) => {
                    return ExpectedWriteOutcome {
                        status: WriteStatus::Err(io::ErrorKind::WriteZero),
                        written,
                    };
                }
                WriteStep::Bytes(n) => {
                    let n = n.min(payload.len() - offset);
                    written.extend_from_slice(&payload[offset..offset + n]);
                    offset += n;
                }
                WriteStep::Error(io::ErrorKind::Interrupted) => {}
                WriteStep::Error(kind) => {
                    return ExpectedWriteOutcome {
                        status: WriteStatus::Err(kind),
                        written,
                    };
                }
            }
        }

        if offset < payload.len() {
            written.extend_from_slice(&payload[offset..]);
        }

        ExpectedWriteOutcome {
            status: WriteStatus::Ok,
            written,
        }
    }

    fn byte_steps(chunks: impl IntoIterator<Item = usize>) -> Vec<WriteStep> {
        chunks.into_iter().map(WriteStep::Bytes).collect()
    }

    fn nonblocking_options() -> SocketOptions {
        SocketOptions {
            send_timeout: Some(std::time::Duration::ZERO),
            ..SocketOptions::default()
        }
    }

    fn socket_with_payload(
        path: WritePath,
        steps: impl IntoIterator<Item = WriteStep>,
        options: SocketOptions,
    ) -> (SocketBase<ScriptedWriteStream>, WriteLog) {
        let stream = ScriptedWriteStream::new(steps);
        let log = stream.log();
        let mut base = SocketBase::new(stream, SocketType::Dealer, options);
        path.buffer_payload(&mut base);
        (base, log)
    }

    async fn assert_short_writes_complete(
        path: WritePath,
        chunks: impl IntoIterator<Item = usize>,
    ) {
        let (mut base, log) =
            socket_with_payload(path, byte_steps(chunks), SocketOptions::default());

        path.write(&mut base).await.unwrap();

        assert_eq!(log.bytes(), PAYLOAD);
        assert!(log.write_count() > 1);
        assert!(base.stream.is_some());
        path.assert_drained(&base);
        assert!(!base.is_poisoned());
    }

    async fn assert_write_failure_after_progress(
        path: WritePath,
        script: impl IntoIterator<Item = WriteStep>,
        expected_kind: io::ErrorKind,
        expected_written: &[u8],
    ) {
        let (mut base, log) = socket_with_payload(path, script, SocketOptions::default());

        let err = path.write(&mut base).await.unwrap_err();

        assert_eq!(err.kind(), expected_kind);
        assert_eq!(log.bytes(), expected_written);
        assert!(base.stream.is_none());
        assert!(base.is_poisoned());
    }

    async fn assert_write_success(path: WritePath, script: impl IntoIterator<Item = WriteStep>) {
        let (mut base, log) = socket_with_payload(path, script, SocketOptions::default());

        path.write(&mut base).await.unwrap();

        assert_eq!(log.bytes(), PAYLOAD);
        assert!(base.stream.is_some());
        path.assert_drained(&base);
        assert!(!base.is_poisoned());
    }

    async fn assert_pre_io_error_preserves_buffer(
        path: WritePath,
        options: SocketOptions,
        stream_state: StreamState,
        expected_kind: io::ErrorKind,
    ) {
        let (mut base, log) = socket_with_payload(path, [], options);
        if matches!(stream_state, StreamState::Disconnected) {
            base.stream = None;
        }

        let err = path.write(&mut base).await.unwrap_err();

        assert_eq!(err.kind(), expected_kind);
        assert!(log.is_empty());
        match stream_state {
            StreamState::Connected => assert!(base.stream.is_some()),
            StreamState::Disconnected => assert!(base.stream.is_none()),
        }
        path.assert_payload_buffered(&base);
        assert!(!base.is_poisoned());
    }

    fn write_step_scripts(max_len: usize) -> Vec<Vec<WriteStep>> {
        const STEPS: &[WriteStep] = &[
            WriteStep::Bytes(0),
            WriteStep::Bytes(1),
            WriteStep::Bytes(2),
            WriteStep::Error(io::ErrorKind::Interrupted),
            WriteStep::Error(io::ErrorKind::BrokenPipe),
        ];

        fn extend_scripts(
            scripts: &mut Vec<Vec<WriteStep>>,
            current: &mut Vec<WriteStep>,
            steps: &[WriteStep],
            max_len: usize,
        ) {
            scripts.push(current.clone());
            if current.len() == max_len {
                return;
            }

            for step in steps {
                current.push(*step);
                extend_scripts(scripts, current, steps, max_len);
                current.pop();
            }
        }

        let mut scripts = Vec::new();
        extend_scripts(&mut scripts, &mut Vec::new(), STEPS, max_len);
        scripts
    }

    fn short_write_scripts(max_len: usize) -> Vec<Vec<WriteStep>> {
        write_step_scripts(max_len)
            .into_iter()
            .filter(|script| matches!(script.first(), Some(WriteStep::Bytes(1 | 2))))
            .collect()
    }

    async fn scripted_write_case_failures(path: WritePath, script: &[WriteStep]) -> Vec<String> {
        let stream = ScriptedWriteStream::new(script.iter().copied());
        let log = stream.log();
        let mut base = SocketBase::new(stream, SocketType::Dealer, SocketOptions::default());
        path.buffer_payload(&mut base);

        let expected = simulate_write_all(PAYLOAD, script);
        let result = path.write(&mut base).await;

        let mut failures = Vec::new();
        let actual_status = WriteStatus::from_result(&result);
        if actual_status != expected.status {
            failures.push(format!(
                "path={path:?} script={script:?}: result {actual_status:?} != {:?}",
                expected.status
            ));
        }

        let actual_written = log.bytes();
        if actual_written != expected.written {
            failures.push(format!(
                "path={path:?} script={script:?}: written {actual_written:?} != {:?}",
                expected.written
            ));
        }

        match expected.status {
            WriteStatus::Ok => {
                if base.stream.is_none() {
                    failures.push(format!(
                        "path={path:?} script={script:?}: stream disconnected after expected success"
                    ));
                }
                if base.is_poisoned() {
                    failures.push(format!(
                        "path={path:?} script={script:?}: socket poisoned after expected success"
                    ));
                }
                match path {
                    WritePath::WriteFromBuf if !base.write_buf.is_empty() => {
                        failures.push(format!(
                            "path={path:?} script={script:?}: write_buf not empty after expected success"
                        ));
                    }
                    WritePath::FlushSendBuffer if !base.send_buffer.is_empty() => {
                        failures.push(format!(
                            "path={path:?} script={script:?}: send_buffer not empty after expected success"
                        ));
                    }
                    WritePath::FlushSendBuffer if base.buffered_messages != 0 => {
                        failures.push(format!(
                            "path={path:?} script={script:?}: buffered_messages {} != 0 after expected success",
                            base.buffered_messages
                        ));
                    }
                    _ => {}
                }
            }
            WriteStatus::Err(_) => {
                if base.stream.is_some() {
                    failures.push(format!(
                        "path={path:?} script={script:?}: stream still connected after expected failure"
                    ));
                }
                if !base.is_poisoned() {
                    failures.push(format!(
                        "path={path:?} script={script:?}: socket not poisoned after expected failure"
                    ));
                }
            }
        }

        failures
    }

    async fn assert_scripted_write_cases(path: WritePath, scripts: Vec<Vec<WriteStep>>) {
        let mut failures = Vec::new();

        for script in scripts {
            failures.extend(scripted_write_case_failures(path, &script).await);
        }

        assert!(
            failures.is_empty(),
            "{} scripted write failures:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    // ── PING / PONG frame builders ────────────────────────────────────────────

    #[test]
    fn test_build_ping_frame_structure() {
        // TTL = 100 tenths = 10 seconds
        let frame = build_ping_frame(100);
        // Byte 0: COMMAND flag (0x04), short frame
        assert_eq!(frame[0], 0x04, "PING frame must have COMMAND flag");
        // Byte 1: body length = 5 (\x04PING) + 2 (TTL) = 7
        assert_eq!(frame[1], 7, "PING body length must be 7 bytes");
        // Bytes 2-6: "\x04PING" (1-byte name-len + "PING")
        assert_eq!(
            &frame[2..7],
            b"\x04PING",
            "PING body must start with \\x04PING"
        );
        // Bytes 7-8: TTL big-endian
        let ttl = u16::from_be_bytes([frame[7], frame[8]]);
        assert_eq!(ttl, 100, "PING TTL field must match the argument");
    }

    #[test]
    fn test_build_pong_frame_structure() {
        let frame = build_pong_frame();
        // Byte 0: COMMAND flag (0x04)
        assert_eq!(frame[0], 0x04, "PONG frame must have COMMAND flag");
        // Byte 1: body length = 5 (\x04PONG = 1-byte length prefix + "PONG")
        assert_eq!(frame[1], 5, "PONG body length must be 5 bytes");
        // Bytes 2-6: "\x04PONG"
        assert_eq!(&frame[2..7], b"\x04PONG", "PONG body must be \\x04PONG");
    }

    #[test]
    fn test_is_ping_payload() {
        assert!(is_ping_payload(b"\x04PING\x00\x0A"));
        assert!(!is_ping_payload(b"\x04PONG"));
        assert!(!is_ping_payload(b"\x05READY"));
        assert!(!is_ping_payload(b""));
    }

    #[test]
    fn test_is_ping_payload_rejects_context_over_16_octets() {
        assert!(!is_ping_payload(b"\x04PING\x00\x0A12345678901234567"));
    }

    #[test]
    fn test_is_pong_payload() {
        assert!(is_pong_payload(b"\x04PONG"));
        assert!(!is_pong_payload(b"\x04PING\x00\x0A"));
        assert!(!is_pong_payload(b"\x05READY"));
        assert!(!is_pong_payload(b""));
    }

    #[test]
    fn test_is_pong_payload_rejects_context_over_16_octets() {
        assert!(!is_pong_payload(b"\x04PONG12345678901234567"));
    }

    // ── Heartbeat state helpers ───────────────────────────────────────────────

    /// `note_recv` must not panic when heartbeating is disabled.
    #[test]
    fn test_note_recv_no_op_when_disabled() {
        // We only test the public helpers  -  SocketBase itself requires a
        // concrete stream type which is difficult to instantiate in unit tests.
        // The logic here is purely tested through the helper functions.
        // Full integration is covered by the heartbeat field initialisation
        // verified in the constructor tests below.
        let _ = build_ping_frame(0); // smoke-test: no panic
        let _ = build_pong_frame(); // smoke-test: no panic
    }

    /// Verify that the PING frame's TTL is encoded as big-endian in tenths of
    /// a second.
    #[test]
    fn test_ping_ttl_encoding() {
        // 300 tenths = 30 seconds
        let frame = build_ping_frame(300);
        let ttl = u16::from_be_bytes([frame[7], frame[8]]);
        assert_eq!(ttl, 300);

        // Zero TTL is also valid (no peer-side timeout)
        let frame0 = build_ping_frame(0);
        let ttl0 = u16::from_be_bytes([frame0[7], frame0[8]]);
        assert_eq!(ttl0, 0);

        // Max u16
        let frame_max = build_ping_frame(u16::MAX);
        let ttl_max = u16::from_be_bytes([frame_max[7], frame_max[8]]);
        assert_eq!(ttl_max, u16::MAX);
    }

    #[test]
    fn test_write_from_buf_retries_short_writes_before_disarming() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_write_from_buf_retries_short_writes_before_disarming_impl());
    }

    async fn test_write_from_buf_retries_short_writes_before_disarming_impl() {
        assert_short_writes_complete(WritePath::WriteFromBuf, [2, 2]).await;
    }

    #[test]
    fn test_flush_send_buffer_retries_short_writes_before_disarming() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_flush_send_buffer_retries_short_writes_before_disarming_impl());
    }

    async fn test_flush_send_buffer_retries_short_writes_before_disarming_impl() {
        assert_short_writes_complete(WritePath::FlushSendBuffer, [2, 2]).await;
    }

    #[test]
    fn test_write_from_buf_write_zero_poisons_and_disconnects() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_write_from_buf_write_zero_poisons_and_disconnects_impl());
    }

    async fn test_write_from_buf_write_zero_poisons_and_disconnects_impl() {
        assert_write_failure_after_progress(
            WritePath::WriteFromBuf,
            [WriteStep::Bytes(2), WriteStep::Bytes(0)],
            io::ErrorKind::WriteZero,
            b"ab",
        )
        .await;
    }

    #[test]
    fn test_flush_send_buffer_write_zero_poisons_and_disconnects() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_flush_send_buffer_write_zero_poisons_and_disconnects_impl());
    }

    async fn test_flush_send_buffer_write_zero_poisons_and_disconnects_impl() {
        assert_write_failure_after_progress(
            WritePath::FlushSendBuffer,
            [WriteStep::Bytes(2), WriteStep::Bytes(0)],
            io::ErrorKind::WriteZero,
            b"ab",
        )
        .await;
    }

    #[test]
    fn test_write_from_buf_write_error_after_progress_poisons_and_disconnects() {
        monocoque_core::rt::LocalRuntime::new().unwrap().block_on(
            test_write_from_buf_write_error_after_progress_poisons_and_disconnects_impl(),
        );
    }

    async fn test_write_from_buf_write_error_after_progress_poisons_and_disconnects_impl() {
        assert_write_failure_after_progress(
            WritePath::WriteFromBuf,
            [
                WriteStep::Bytes(2),
                WriteStep::Error(io::ErrorKind::BrokenPipe),
            ],
            io::ErrorKind::BrokenPipe,
            b"ab",
        )
        .await;
    }

    #[test]
    fn test_flush_send_buffer_write_error_after_progress_poisons_and_disconnects() {
        monocoque_core::rt::LocalRuntime::new().unwrap().block_on(
            test_flush_send_buffer_write_error_after_progress_poisons_and_disconnects_impl(),
        );
    }

    async fn test_flush_send_buffer_write_error_after_progress_poisons_and_disconnects_impl() {
        assert_write_failure_after_progress(
            WritePath::FlushSendBuffer,
            [
                WriteStep::Bytes(2),
                WriteStep::Error(io::ErrorKind::BrokenPipe),
            ],
            io::ErrorKind::BrokenPipe,
            b"ab",
        )
        .await;
    }

    #[test]
    fn test_write_from_buf_interrupted_write_retries_without_poisoning() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_write_from_buf_interrupted_write_retries_without_poisoning_impl());
    }

    async fn test_write_from_buf_interrupted_write_retries_without_poisoning_impl() {
        assert_write_success(
            WritePath::WriteFromBuf,
            [
                WriteStep::Error(io::ErrorKind::Interrupted),
                WriteStep::Bytes(2),
                WriteStep::Bytes(4),
            ],
        )
        .await;
    }

    #[test]
    fn test_flush_send_buffer_interrupted_write_retries_without_poisoning() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_flush_send_buffer_interrupted_write_retries_without_poisoning_impl());
    }

    async fn test_flush_send_buffer_interrupted_write_retries_without_poisoning_impl() {
        assert_write_success(
            WritePath::FlushSendBuffer,
            [
                WriteStep::Error(io::ErrorKind::Interrupted),
                WriteStep::Bytes(2),
                WriteStep::Bytes(4),
            ],
        )
        .await;
    }

    #[test]
    fn test_scripted_write_from_buf_short_write_sequences_match_socket_state() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_scripted_write_from_buf_short_write_sequences_match_socket_state_impl());
    }

    async fn test_scripted_write_from_buf_short_write_sequences_match_socket_state_impl() {
        assert_scripted_write_cases(WritePath::WriteFromBuf, short_write_scripts(4)).await;
    }

    #[test]
    fn test_scripted_flush_send_buffer_short_write_sequences_match_socket_state() {
        monocoque_core::rt::LocalRuntime::new().unwrap().block_on(
            test_scripted_flush_send_buffer_short_write_sequences_match_socket_state_impl(),
        );
    }

    async fn test_scripted_flush_send_buffer_short_write_sequences_match_socket_state_impl() {
        assert_scripted_write_cases(WritePath::FlushSendBuffer, short_write_scripts(4)).await;
    }

    #[test]
    fn test_nonblocking_write_from_buf_keeps_buffer_and_health() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_nonblocking_write_from_buf_keeps_buffer_and_health_impl());
    }

    async fn test_nonblocking_write_from_buf_keeps_buffer_and_health_impl() {
        assert_pre_io_error_preserves_buffer(
            WritePath::WriteFromBuf,
            nonblocking_options(),
            StreamState::Connected,
            io::ErrorKind::WouldBlock,
        )
        .await;
    }

    #[test]
    fn test_write_from_buf_not_connected_does_not_poison() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_write_from_buf_not_connected_does_not_poison_impl());
    }

    async fn test_write_from_buf_not_connected_does_not_poison_impl() {
        assert_pre_io_error_preserves_buffer(
            WritePath::WriteFromBuf,
            SocketOptions::default(),
            StreamState::Disconnected,
            io::ErrorKind::NotConnected,
        )
        .await;
    }

    #[test]
    fn test_write_from_buf_not_connected_takes_precedence_over_nonblocking() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_write_from_buf_not_connected_takes_precedence_over_nonblocking_impl());
    }

    async fn test_write_from_buf_not_connected_takes_precedence_over_nonblocking_impl() {
        assert_pre_io_error_preserves_buffer(
            WritePath::WriteFromBuf,
            nonblocking_options(),
            StreamState::Disconnected,
            io::ErrorKind::NotConnected,
        )
        .await;
    }

    #[test]
    fn test_nonblocking_flush_keeps_buffer_and_health() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_nonblocking_flush_keeps_buffer_and_health_impl());
    }

    async fn test_nonblocking_flush_keeps_buffer_and_health_impl() {
        assert_pre_io_error_preserves_buffer(
            WritePath::FlushSendBuffer,
            nonblocking_options(),
            StreamState::Connected,
            io::ErrorKind::WouldBlock,
        )
        .await;
    }

    #[test]
    fn test_flush_send_buffer_not_connected_keeps_buffer_and_health() {
        monocoque_core::rt::LocalRuntime::new()
            .unwrap()
            .block_on(test_flush_send_buffer_not_connected_keeps_buffer_and_health_impl());
    }

    async fn test_flush_send_buffer_not_connected_keeps_buffer_and_health_impl() {
        assert_pre_io_error_preserves_buffer(
            WritePath::FlushSendBuffer,
            SocketOptions::default(),
            StreamState::Disconnected,
            io::ErrorKind::NotConnected,
        )
        .await;
    }

    #[test]
    fn test_flush_send_buffer_not_connected_takes_precedence_over_nonblocking() {
        monocoque_core::rt::LocalRuntime::new().unwrap().block_on(
            test_flush_send_buffer_not_connected_takes_precedence_over_nonblocking_impl(),
        );
    }

    async fn test_flush_send_buffer_not_connected_takes_precedence_over_nonblocking_impl() {
        assert_pre_io_error_preserves_buffer(
            WritePath::FlushSendBuffer,
            nonblocking_options(),
            StreamState::Disconnected,
            io::ErrorKind::NotConnected,
        )
        .await;
    }
}
