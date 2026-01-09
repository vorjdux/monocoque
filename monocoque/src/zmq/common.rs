//! Common utilities for ZeroMQ socket implementations.

use std::io;

/// Convert a channel send error to an IO error.
///
/// Used by all socket types to convert flume channel errors
/// into standard IO errors with BrokenPipe kind.
pub fn channel_to_io_error<T, E>(result: Result<T, E>) -> io::Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    result.map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
}
