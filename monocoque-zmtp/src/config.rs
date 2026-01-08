//! ZMTP configuration and buffer sizing
//!
//! This module provides configuration for buffer sizes used across ZMTP sockets.
//! Tuning these values can significantly impact performance based on workload.

/// Default read buffer size (8KB)
///
/// Used for arena-allocated read buffers. Tune based on expected message sizes:
/// - Small messages (< 1KB): 4096 bytes sufficient
/// - Medium messages (1-8KB): 8192 bytes (default)
/// - Large messages (> 8KB): 16384 or 32768 bytes
pub const DEFAULT_READ_BUF_SIZE: usize = 8192;

/// Default write buffer size (8KB)
///
/// Used for BytesMut write buffers. Should match typical encoded message size.
pub const DEFAULT_WRITE_BUF_SIZE: usize = 8192;

/// Small read buffer size (4KB)
///
/// Optimized for REQ/REP with small messages (< 1KB).
pub const SMALL_READ_BUF_SIZE: usize = 4096;

/// Small write buffer size (4KB)
///
/// Optimized for encoding small messages.
pub const SMALL_WRITE_BUF_SIZE: usize = 4096;

/// Large read buffer size (16KB)
///
/// Optimized for DEALER/ROUTER with larger messages (8-16KB).
pub const LARGE_READ_BUF_SIZE: usize = 16384;

/// Large write buffer size (16KB)
///
/// Optimized for encoding larger messages.
pub const LARGE_WRITE_BUF_SIZE: usize = 16384;

/// Initial staging buffer capacity for decoder reassembly (256 bytes)
///
/// Pre-allocated to avoid initial reallocation on fragmented frames.
/// Only used when frame spans multiple segments (slow path).
pub const STAGING_BUF_INITIAL_CAP: usize = 256;

/// Socket buffer configuration
#[derive(Debug, Clone, Copy)]
pub struct BufferConfig {
    /// Read buffer size (arena allocation)
    pub read_buf_size: usize,
    /// Write buffer size (BytesMut capacity)
    pub write_buf_size: usize,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            read_buf_size: DEFAULT_READ_BUF_SIZE,
            write_buf_size: DEFAULT_WRITE_BUF_SIZE,
        }
    }
}

impl BufferConfig {
    /// Configuration optimized for small messages (< 1KB)
    ///
    /// Best for REQ/REP ping-pong patterns.
    #[must_use]
    pub const fn small() -> Self {
        Self {
            read_buf_size: SMALL_READ_BUF_SIZE,
            write_buf_size: SMALL_WRITE_BUF_SIZE,
        }
    }

    /// Configuration optimized for large messages (8-16KB)
    ///
    /// Best for DEALER/ROUTER with larger payloads.
    #[must_use]
    pub const fn large() -> Self {
        Self {
            read_buf_size: LARGE_READ_BUF_SIZE,
            write_buf_size: LARGE_WRITE_BUF_SIZE,
        }
    }

    /// Custom buffer configuration
    #[must_use]
    pub const fn custom(read_buf_size: usize, write_buf_size: usize) -> Self {
        Self {
            read_buf_size,
            write_buf_size,
        }
    }
}
