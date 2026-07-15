//! TCP utilities for high-performance networking.
//!
//! This module provides generic TCP optimizations that are protocol-agnostic.
//!
//! # Safety
//!
//! This module uses unsafe code to access raw file descriptors/sockets for
//! TCP socket configuration. The unsafe operations are encapsulated and safe
//! to use from the public API.

#![allow(unsafe_code)]

use std::io;

/// Enable `TCP_NODELAY` on a TCP stream for minimal latency.
///
/// This disables Nagle's algorithm, trading bandwidth efficiency for lower latency.
/// Essential for request-reply patterns and real-time messaging.
///
/// Generic over the stream type so it works with whichever runtime backend is
/// active: any stream exposing a raw socket handle qualifies.
///
/// # Platform Support
///
/// Supported on Unix (Linux, macOS) and Windows platforms.
///
/// # Errors
///
/// Returns an error if the socket option cannot be set.
#[cfg(unix)]
#[inline]
pub fn enable_tcp_nodelay<S: std::os::unix::io::AsRawFd>(stream: &S) -> io::Result<()> {
    use std::os::unix::io::FromRawFd;
    let fd = stream.as_raw_fd();
    let sock = unsafe { socket2::Socket::from_raw_fd(fd) };
    sock.set_nodelay(true)?;
    std::mem::forget(sock); // Don't close the fd
    Ok(())
}

/// Enable `TCP_NODELAY` on a TCP stream for minimal latency.
///
/// See the Unix variant for details.
///
/// # Errors
///
/// Returns an error if the socket option cannot be set.
#[cfg(windows)]
#[inline]
pub fn enable_tcp_nodelay<S: std::os::windows::io::AsRawSocket>(stream: &S) -> io::Result<()> {
    use std::os::windows::io::FromRawSocket;
    let raw = stream.as_raw_socket();
    let sock = unsafe { socket2::Socket::from_raw_socket(raw) };
    sock.set_nodelay(true)?;
    std::mem::forget(sock); // Don't close the socket
    Ok(())
}

/// Enable `TCP_NODELAY` on a TCP stream (no-op on unsupported platforms).
///
/// # Errors
///
/// Never returns an error on this platform.
#[cfg(not(any(unix, windows)))]
#[inline]
pub fn enable_tcp_nodelay<S>(_stream: &S) -> io::Result<()> {
    Ok(())
}

/// Build a listening `std::net::TcpListener` bound to `addr` with `SO_REUSEPORT`
/// (and `SO_REUSEADDR`) set before the bind.
///
/// `SO_REUSEPORT` must be set on the socket before `bind`, so this constructs
/// the socket via socket2, sets the option, then binds and listens. Multiple
/// listeners on the same address can then coexist with in-kernel load balancing
/// across them, which is the path to scaling accept in high-connection ROUTER,
/// PULL, and PUB. Each runtime backend adopts the returned std listener via its
/// own `from_std`.
///
/// # Errors
///
/// Returns an error if the socket cannot be created, the option set, or the
/// address bound.
#[cfg(unix)]
pub fn reuseport_listener(addr: std::net::SocketAddr) -> io::Result<std::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    sock.set_reuse_address(true)?;
    sock.set_reuse_port(true)?;
    sock.bind(&addr.into())?;
    sock.listen(1024)?;
    Ok(sock.into())
}

/// `SO_REUSEPORT` is a Unix (Linux/BSD) socket option; unsupported elsewhere.
///
/// # Errors
///
/// Always returns `Unsupported` on non-Unix platforms.
#[cfg(not(unix))]
pub fn reuseport_listener(_addr: std::net::SocketAddr) -> io::Result<std::net::TcpListener> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SO_REUSEPORT is only supported on Unix",
    ))
}

/// Configure the OS-level socket send/receive buffer sizes (`SO_SNDBUF` /
/// `SO_RCVBUF`) on a TCP stream.
///
/// A value of `0` (or negative) leaves the corresponding buffer at the OS
/// default. On high bandwidth-delay-product links the kernel socket buffer size
/// is what caps single-flow throughput, so exposing this is both a correctness
/// and a performance lever.
///
/// Note: the kernel may round or clamp the requested size, and Linux typically
/// reports back roughly double the requested value from `getsockopt`.
///
/// # Errors
///
/// Returns an error if the socket options cannot be set.
#[cfg(unix)]
pub fn configure_socket_buffers<S: std::os::unix::io::AsRawFd>(
    stream: &S,
    sndbuf: i32,
    rcvbuf: i32,
) -> io::Result<()> {
    use std::os::unix::io::FromRawFd;

    if sndbuf <= 0 && rcvbuf <= 0 {
        return Ok(());
    }

    let fd = stream.as_raw_fd();
    let sock = unsafe { socket2::Socket::from_raw_fd(fd) };

    let result = (|| {
        if sndbuf > 0 {
            sock.set_send_buffer_size(sndbuf as usize)?;
        }
        if rcvbuf > 0 {
            sock.set_recv_buffer_size(rcvbuf as usize)?;
        }
        Ok(())
    })();

    std::mem::forget(sock); // Don't close the fd
    result
}

/// Configure the OS-level socket send/receive buffer sizes (Windows).
///
/// See the Unix variant for details.
///
/// # Errors
///
/// Returns an error if the socket options cannot be set.
#[cfg(windows)]
pub fn configure_socket_buffers<S: std::os::windows::io::AsRawSocket>(
    stream: &S,
    sndbuf: i32,
    rcvbuf: i32,
) -> io::Result<()> {
    use std::os::windows::io::FromRawSocket;

    if sndbuf <= 0 && rcvbuf <= 0 {
        return Ok(());
    }

    let raw = stream.as_raw_socket();
    let sock = unsafe { socket2::Socket::from_raw_socket(raw) };

    let result = (|| {
        if sndbuf > 0 {
            sock.set_send_buffer_size(sndbuf as usize)?;
        }
        if rcvbuf > 0 {
            sock.set_recv_buffer_size(rcvbuf as usize)?;
        }
        Ok(())
    })();

    std::mem::forget(sock); // Don't close the socket
    result
}

/// Configure the OS-level socket buffer sizes (unsupported platforms).
///
/// # Errors
///
/// Never returns an error on this platform.
#[cfg(not(any(unix, windows)))]
pub fn configure_socket_buffers<S>(_stream: &S, sndbuf: i32, rcvbuf: i32) -> io::Result<()> {
    let _ = (sndbuf, rcvbuf);
    Ok(())
}

/// Configure TCP keepalive settings on a TCP stream.
///
/// This enables connection monitoring for long-lived connections, helping detect
/// dead peers and network failures. Generic over the stream type so it works with
/// whichever runtime backend is active.
///
/// # Arguments
///
/// * `stream` - The TCP stream to configure
/// * `keepalive` - -1 for OS default, 0 to disable, 1 to enable
/// * `keepalive_cnt` - Number of probes before timeout (-1 for OS default)
/// * `keepalive_idle` - Seconds before first probe (-1 for OS default)
/// * `keepalive_intvl` - Seconds between probes (-1 for OS default)
///
/// # Platform Support
///
/// Supported on Unix (Linux, macOS) and Windows platforms. On Windows, some options
/// may have limited support.
///
/// # Errors
///
/// Returns an error if the socket options cannot be set.
#[cfg(unix)]
pub fn configure_tcp_keepalive<S: std::os::unix::io::AsRawFd>(
    stream: &S,
    keepalive: i32,
    keepalive_cnt: i32,
    keepalive_idle: i32,
    keepalive_intvl: i32,
) -> io::Result<()> {
    {
        use std::os::unix::io::FromRawFd;
        use std::time::Duration;

        let fd = stream.as_raw_fd();
        let sock = unsafe { socket2::Socket::from_raw_fd(fd) };

        // Set TCP keepalive on/off
        if keepalive >= 0 {
            sock.set_keepalive(keepalive != 0)?;
        }

        // Configure keepalive parameters if enabled
        if keepalive == 1 {
            let mut params = socket2::TcpKeepalive::new();

            // Idle time before first probe
            if keepalive_idle > 0 {
                params = params.with_time(Duration::from_secs(keepalive_idle as u64));
            }

            // Interval between probes
            #[cfg(not(target_os = "openbsd"))]
            if keepalive_intvl > 0 {
                params = params.with_interval(Duration::from_secs(keepalive_intvl as u64));
            }

            // Number of probes (Linux only)
            #[cfg(any(target_os = "linux", target_os = "android"))]
            if keepalive_cnt > 0 {
                params = params.with_retries(keepalive_cnt as u32);
            }

            sock.set_tcp_keepalive(&params)?;
        }

        std::mem::forget(sock); // Don't close the fd
        Ok(())
    }
}

/// Configure TCP keepalive settings on a TCP stream (Windows).
///
/// See the Unix variant for argument details.
///
/// # Errors
///
/// Returns an error if the socket options cannot be set.
#[cfg(windows)]
pub fn configure_tcp_keepalive<S: std::os::windows::io::AsRawSocket>(
    stream: &S,
    keepalive: i32,
    keepalive_cnt: i32,
    keepalive_idle: i32,
    keepalive_intvl: i32,
) -> io::Result<()> {
    let _ = keepalive_cnt;
    use std::os::windows::io::FromRawSocket;
    use std::time::Duration;

    let raw = stream.as_raw_socket();
    let sock = unsafe { socket2::Socket::from_raw_socket(raw) };

    // Set TCP keepalive on/off
    if keepalive >= 0 {
        sock.set_keepalive(keepalive != 0)?;
    }

    // Configure keepalive parameters if enabled
    if keepalive == 1 {
        let mut params = socket2::TcpKeepalive::new();

        // Windows supports time and interval
        if keepalive_idle > 0 {
            params = params.with_time(Duration::from_secs(keepalive_idle as u64));
        }
        if keepalive_intvl > 0 {
            params = params.with_interval(Duration::from_secs(keepalive_intvl as u64));
        }

        sock.set_tcp_keepalive(&params)?;
    }

    std::mem::forget(sock); // Don't close the socket
    Ok(())
}

/// Configure TCP keepalive settings on a TCP stream (unsupported platforms).
///
/// # Errors
///
/// Never returns an error on this platform.
#[cfg(not(any(unix, windows)))]
pub fn configure_tcp_keepalive<S>(
    _stream: &S,
    keepalive: i32,
    keepalive_cnt: i32,
    keepalive_idle: i32,
    keepalive_intvl: i32,
) -> io::Result<()> {
    // Unsupported platform - just continue
    let _ = (keepalive, keepalive_cnt, keepalive_idle, keepalive_intvl);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn configure_socket_buffers_applies_requested_sizes() {
        use socket2::Socket;
        use std::os::unix::io::{AsRawFd, FromRawFd};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();

        let requested = 256 * 1024;
        configure_socket_buffers(&client, requested, requested).unwrap();

        // Read the sizes back off the fd. The kernel may round or (on Linux)
        // roughly double the requested value, so assert it grew rather than an
        // exact match.
        let fd = client.as_raw_fd();
        let sock = unsafe { Socket::from_raw_fd(fd) };
        let snd = sock.send_buffer_size().unwrap();
        let rcv = sock.recv_buffer_size().unwrap();
        std::mem::forget(sock);

        assert!(
            snd >= requested as usize,
            "SO_SNDBUF not applied: got {snd}, requested {requested}"
        );
        assert!(
            rcv >= requested as usize,
            "SO_RCVBUF not applied: got {rcv}, requested {requested}"
        );
    }

    #[test]
    fn reuseport_allows_two_listeners_on_same_port() {
        // Bind one REUSEPORT listener on an OS-assigned port, then bind a second
        // on the SAME address. Without SO_REUSEPORT the second bind fails with
        // AddrInUse; with it, both coexist (the kernel load-balances accepts).
        let l1 = reuseport_listener("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = l1.local_addr().unwrap();
        let l2 = reuseport_listener(addr).expect("second REUSEPORT bind on same port");
        assert_eq!(
            l1.local_addr().unwrap().port(),
            l2.local_addr().unwrap().port()
        );
    }

    #[test]
    fn configure_socket_buffers_zero_is_noop() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();

        // 0 / negative means "leave OS default" and must not error.
        configure_socket_buffers(&client, 0, 0).unwrap();
        configure_socket_buffers(&client, -1, -1).unwrap();
    }
}
