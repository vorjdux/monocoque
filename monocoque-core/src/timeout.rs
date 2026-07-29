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
            // A zero budget cannot complete an exact read. These helpers serve
            // the handshake, where that means the step is out of time; report a
            // timeout rather than pretend a non-blocking read is in progress.
            // (User-facing RCVTIMEO=0 non-blocking recv is handled in
            // SocketBase::read_raw, which returns WouldBlock after draining any
            // already-buffered frames - it never reaches this helper.)
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Read operation timed out (zero timeout budget)",
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
            // A zero budget cannot complete a full write. See the read helper:
            // in the handshake this means the step is out of time. User-facing
            // SNDTIMEO=0 non-blocking send is handled in SocketBase, not here.
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Write operation timed out (zero timeout budget)",
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
