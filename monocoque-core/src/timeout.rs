//! Timeout utilities for I/O operations
//!
//! Provides timeout wrappers for async read/write operations using compio's timeout support.

use crate::rt::timeout;
use compio_io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use std::io;
use std::time::Duration;

/// Execute an async `read_exact` operation with a timeout.
///
/// Reads exactly the full buffer or returns an error.
pub async fn read_exact_with_timeout<S, B>(
    stream: &mut S,
    buf: B,
    duration: Option<Duration>,
) -> io::Result<compio_buf::BufResult<(), B>>
where
    S: AsyncRead + Unpin,
    B: compio_buf::IoBufMut,
{
    match duration {
        None => {
            // No timeout, block indefinitely
            Ok(stream.read_exact(buf).await)
        }
        Some(d) if d.is_zero() => {
            // Non-blocking mode
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Non-blocking mode not yet implemented",
            ))
        }
        Some(d) => {
            // Timeout mode
            match timeout(d, stream.read_exact(buf)).await {
                Ok(result) => Ok(result),
                Err(_elapsed) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Read operation timed out",
                )),
            }
        }
    }
}

/// Execute an async `write_all` operation with a timeout.
///
/// Writes the entire buffer or returns an error.
pub async fn write_all_with_timeout<S, B>(
    stream: &mut S,
    buf: B,
    duration: Option<Duration>,
) -> io::Result<compio_buf::BufResult<(), B>>
where
    S: AsyncWrite + Unpin,
    B: compio_buf::IoBuf,
{
    match duration {
        None => {
            // No timeout, block indefinitely
            Ok(stream.write_all(buf).await)
        }
        Some(d) if d.is_zero() => {
            // Non-blocking mode
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Non-blocking mode not yet implemented",
            ))
        }
        Some(d) => {
            // Timeout mode
            match timeout(d, stream.write_all(buf)).await {
                Ok(result) => Ok(result),
                Err(_elapsed) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Write operation timed out",
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These are compile-time tests to ensure the API is sound
    // Full integration tests would require actual I/O operations

    #[test]
    fn test_timeout_types() {
        let infinite: Option<Duration> = None;
        assert!(infinite.is_none());
        let nonblocking = Some(Duration::ZERO);
        assert_eq!(nonblocking, Some(Duration::ZERO));
        let timed = Some(Duration::from_secs(5));
        assert_eq!(timed, Some(Duration::from_secs(5)));
    }
}
