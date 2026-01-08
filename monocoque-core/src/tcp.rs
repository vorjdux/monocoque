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

/// Enable TCP_NODELAY on a compio TcpStream for minimal latency.
///
/// This disables Nagle's algorithm, trading bandwidth efficiency for lower latency.
/// Essential for request-reply patterns and real-time messaging.
///
/// # Platform Support
///
/// Supported on Unix (Linux, macOS) and Windows platforms.
///
/// # Errors
///
/// Returns an error if the socket option cannot be set.
#[inline]
pub fn enable_tcp_nodelay(stream: &compio::net::TcpStream) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::io::{AsRawFd, FromRawFd};
        let fd = stream.as_raw_fd();
        let sock = unsafe { socket2::Socket::from_raw_fd(fd) };
        sock.set_nodelay(true)?;
        std::mem::forget(sock); // Don't close the fd
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawSocket, FromRawSocket};
        let raw = stream.as_raw_socket();
        let sock = unsafe { socket2::Socket::from_raw_socket(raw) };
        sock.set_nodelay(true)?;
        std::mem::forget(sock); // Don't close the socket
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Unsupported platform - just continue without TCP_NODELAY
        Ok(())
    }
}
