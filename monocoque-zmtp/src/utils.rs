use bytes::{BufMut, Bytes, BytesMut};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::TcpStream;
use std::io;
use tracing::debug;

/// ZMTP frame flags
pub const FLAG_LONG: u8 = 0x02;
pub const FLAG_COMMAND: u8 = 0x04;

/// How long to pause after an `accept` fails with per-process/system file
/// descriptor exhaustion before returning, so a caller's `loop { accept() }`
/// cannot livelock and burn a core while no descriptors are available.
pub const ACCEPT_FD_EXHAUSTION_BACKOFF: std::time::Duration = std::time::Duration::from_millis(10);

/// Returns true if `err` indicates file descriptor exhaustion: `EMFILE` (the
/// process hit its fd limit) or `ENFILE` (the system-wide table is full).
///
/// These are transient: under fd exhaustion `accept` returns the same error
/// immediately on every call, so a tight accept loop spins at full CPU (the
/// classic accept livelock). Callers should back off before retrying.
#[cfg(unix)]
#[must_use]
pub fn is_fd_exhaustion(err: &io::Error) -> bool {
    // EMFILE = 24, ENFILE = 23 on Linux and the BSDs/macOS.
    matches!(err.raw_os_error(), Some(23) | Some(24))
}

/// Non-Unix platforms do not surface `EMFILE`/`ENFILE`; never treat an error as
/// fd exhaustion.
#[cfg(not(unix))]
#[must_use]
pub fn is_fd_exhaustion(_err: &io::Error) -> bool {
    false
}

/// Back off briefly when an accept error is fd exhaustion.
///
/// Call this on the error path of an accept loop, passing the error, so that
/// `EMFILE`/`ENFILE` throttles the loop instead of spinning. No-op for any other
/// error.
pub async fn backoff_on_fd_exhaustion(err: &io::Error) {
    if is_fd_exhaustion(err) {
        monocoque_core::rt::sleep(ACCEPT_FD_EXHAUSTION_BACKOFF).await;
    }
}

/// Encode a complete ZMTP frame (header + body).
///
/// This function is used ONLY for protocol commands (READY, etc.) which are
/// small, infrequent messages sent during handshake. For user data messages,
/// use zero-copy transmission via the codec.
///
/// # Errors
///
/// Returns an error if the body length exceeds ZMTP frame size limits.
pub fn encode_frame(flags: u8, body: &Bytes) -> Bytes {
    let len = body.len();
    let header_len = if len <= 255 { 2 } else { 9 };
    let mut out = BytesMut::with_capacity(header_len + len);

    if len <= 255 {
        out.put_u8(flags & !FLAG_LONG);
        out.put_u8(len as u8);
    } else {
        out.put_u8(flags | FLAG_LONG);
        out.put_u64(len as u64);
    }

    out.extend_from_slice(body);
    out.freeze()
}

/// Build a READY command body (ZMTP/37).
///
/// Grammar:
/// - 1 byte: command name length
/// - "READY"
/// - Repeated properties:
///   - 1 byte: property name length
///   - property name
///   - 4 bytes: value length (BE)
///   - value
///
/// Mandatory:
/// - Socket-Type
///
/// Optional:
/// - Identity
pub fn build_ready(socket_type: &str, identity: Option<&[u8]>) -> Bytes {
    let mut body = BytesMut::new();

    // Command name
    body.put_u8(5);
    body.extend_from_slice(b"READY");

    // Mandatory: Socket-Type
    put_property(&mut body, "Socket-Type", socket_type.as_bytes());

    // Optional: Identity
    if let Some(id) = identity {
        put_property(&mut body, "Identity", id);
    }

    body.freeze()
}

/// Helper: encode a READY property
#[inline]
fn put_property(dst: &mut BytesMut, name: &str, value: &[u8]) {
    let name_bytes = name.as_bytes();

    dst.put_u8(name_bytes.len() as u8);
    dst.extend_from_slice(name_bytes);

    dst.put_u32(value.len() as u32);
    dst.extend_from_slice(value);
}

/// Configure TCP stream with optimizations (TCP_NODELAY, keepalive).
///
/// Applies TCP socket options based on SocketOptions configuration:
/// - Always enables TCP_NODELAY for low latency
/// - Configures TCP keepalive if enabled in options
///
/// # Arguments
///
/// * `stream` - The TCP stream to configure
/// * `options` - Socket options containing TCP configuration
/// * `socket_name` - Name for debug logging (e.g., "DEALER", "ROUTER")
///
/// # Errors
///
/// Returns an error if socket options cannot be applied.
pub fn configure_tcp_stream(
    stream: &TcpStream,
    options: &SocketOptions,
    socket_name: &str,
) -> io::Result<()> {
    // Enable TCP_NODELAY for low latency
    monocoque_core::tcp::enable_tcp_nodelay(stream)?;
    debug!("[{}] TCP_NODELAY enabled", socket_name);

    // Apply OS-level socket buffer sizes (SO_SNDBUF / SO_RCVBUF) when set.
    // A value of 0 leaves the kernel default in place.
    if options.sndbuf > 0 || options.rcvbuf > 0 {
        monocoque_core::tcp::configure_socket_buffers(stream, options.sndbuf, options.rcvbuf)?;
        debug!(
            "[{}] socket buffers set (sndbuf={}, rcvbuf={})",
            socket_name, options.sndbuf, options.rcvbuf
        );
    }

    // Configure TCP keepalive if specified
    monocoque_core::tcp::configure_tcp_keepalive(
        stream,
        options.tcp_keepalive,
        options.tcp_keepalive_cnt,
        options.tcp_keepalive_idle,
        options.tcp_keepalive_intvl,
    )?;

    if options.tcp_keepalive == 1 {
        debug!(
            "[{}] TCP keepalive enabled (cnt={}, idle={}s, intvl={}s)",
            socket_name,
            options.tcp_keepalive_cnt,
            options.tcp_keepalive_idle,
            options.tcp_keepalive_intvl
        );
    }

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn fd_exhaustion_errnos_are_recognized() {
        // EMFILE (24) and ENFILE (23) are fd exhaustion.
        assert!(is_fd_exhaustion(&io::Error::from_raw_os_error(24)));
        assert!(is_fd_exhaustion(&io::Error::from_raw_os_error(23)));
    }

    #[test]
    fn other_errors_are_not_fd_exhaustion() {
        assert!(!is_fd_exhaustion(&io::Error::from_raw_os_error(104))); // ECONNRESET
        assert!(!is_fd_exhaustion(&io::Error::new(
            io::ErrorKind::WouldBlock,
            "would block"
        )));
        // An error with no OS errno (e.g. a synthetic one) is not fd exhaustion.
        assert!(!is_fd_exhaustion(&io::Error::other("synthetic")));
    }
}
