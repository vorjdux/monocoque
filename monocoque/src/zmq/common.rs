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

/// Parse a TCP endpoint string into a `SocketAddr`.
///
/// Accepts both URL form (`tcp://127.0.0.1:5555`) and bare form (`127.0.0.1:5555`).
pub(super) fn parse_tcp_endpoint(endpoint: &str) -> io::Result<std::net::SocketAddr> {
    if let Ok(monocoque_core::endpoint::Endpoint::Tcp(addr)) =
        monocoque_core::endpoint::Endpoint::parse(endpoint)
    {
        Ok(addr)
    } else {
        endpoint
            .parse::<std::net::SocketAddr>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }
}
