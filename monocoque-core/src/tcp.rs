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

/// Enable `TCP_NODELAY` on a compio `TcpStream` for minimal latency.
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

/// Configure TCP keepalive settings on a compio `TcpStream`.
///
/// This enables connection monitoring for long-lived connections, helping detect
/// dead peers and network failures.
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
pub fn configure_tcp_keepalive(
    stream: &compio::net::TcpStream,
    keepalive: i32,
    keepalive_cnt: i32,
    keepalive_idle: i32,
    keepalive_intvl: i32,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::io::{AsRawFd, FromRawFd};
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

    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawSocket, FromRawSocket};
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

    #[cfg(not(any(unix, windows)))]
    {
        // Unsupported platform - just continue
        let _ = (keepalive, keepalive_cnt, keepalive_idle, keepalive_intvl);
        Ok(())
    }
}
