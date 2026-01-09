//! Endpoint abstraction for transport-agnostic socket addressing.
//!
//! Provides unified addressing for TCP and IPC transports with parsing support.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

/// Transport endpoint address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// TCP transport: `tcp://host:port`
    Tcp(SocketAddr),
    /// IPC transport (Unix domain socket): `ipc:///path/to/socket`
    #[cfg(unix)]
    Ipc(PathBuf),
}

impl Endpoint {
    /// Parse an endpoint from a string.
    ///
    /// Supported formats:
    /// - `tcp://127.0.0.1:5555`
    /// - `tcp://[::1]:5555` (IPv6)
    /// - `ipc:///tmp/socket.sock` (Unix only)
    ///
    /// # Examples
    ///
    /// ```
    /// use monocoque_core::endpoint::Endpoint;
    ///
    /// let endpoint = Endpoint::parse("tcp://127.0.0.1:5555").unwrap();
    /// assert!(matches!(endpoint, Endpoint::Tcp(_)));
    ///
    /// # #[cfg(unix)]
    /// # {
    /// let endpoint = Endpoint::parse("ipc:///tmp/test.sock").unwrap();
    /// assert!(matches!(endpoint, Endpoint::Ipc(_)));
    /// # }
    /// ```
    pub fn parse(s: &str) -> Result<Self, EndpointError> {
        s.parse()
    }

    /// Returns true if this is a TCP endpoint.
    pub fn is_tcp(&self) -> bool {
        matches!(self, Endpoint::Tcp(_))
    }

    /// Returns true if this is an IPC endpoint.
    #[cfg(unix)]
    pub fn is_ipc(&self) -> bool {
        matches!(self, Endpoint::Ipc(_))
    }
}

impl FromStr for Endpoint {
    type Err = EndpointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(addr) = s.strip_prefix("tcp://") {
            let socket_addr = addr
                .parse::<SocketAddr>()
                .map_err(|_| EndpointError::InvalidTcpAddress(addr.to_string()))?;
            Ok(Endpoint::Tcp(socket_addr))
        } else if let Some(path) = s.strip_prefix("ipc://") {
            #[cfg(unix)]
            {
                Ok(Endpoint::Ipc(PathBuf::from(path)))
            }
            #[cfg(not(unix))]
            {
                Err(EndpointError::IpcNotSupported)
            }
        } else {
            Err(EndpointError::InvalidScheme(s.to_string()))
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Tcp(addr) => write!(f, "tcp://{}", addr),
            #[cfg(unix)]
            Endpoint::Ipc(path) => write!(f, "ipc://{}", path.display()),
        }
    }
}

/// Errors that can occur when parsing or using endpoints.
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("Invalid scheme in endpoint: {0} (expected tcp:// or ipc://)")]
    InvalidScheme(String),

    #[error("Invalid TCP address: {0}")]
    InvalidTcpAddress(String),

    #[error("IPC transport not supported on this platform")]
    IpcNotSupported,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tcp_ipv4() {
        let endpoint = Endpoint::parse("tcp://127.0.0.1:5555").unwrap();
        assert!(matches!(endpoint, Endpoint::Tcp(_)));
        assert_eq!(endpoint.to_string(), "tcp://127.0.0.1:5555");
    }

    #[test]
    fn test_parse_tcp_ipv6() {
        let endpoint = Endpoint::parse("tcp://[::1]:5555").unwrap();
        assert!(matches!(endpoint, Endpoint::Tcp(_)));
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_ipc() {
        let endpoint = Endpoint::parse("ipc:///tmp/test.sock").unwrap();
        assert!(matches!(endpoint, Endpoint::Ipc(_)));
        assert_eq!(endpoint.to_string(), "ipc:///tmp/test.sock");
    }

    #[test]
    fn test_invalid_scheme() {
        let result = Endpoint::parse("http://127.0.0.1:5555");
        assert!(matches!(result, Err(EndpointError::InvalidScheme(_))));
    }

    #[test]
    fn test_invalid_tcp_address() {
        let result = Endpoint::parse("tcp://invalid:port");
        assert!(matches!(result, Err(EndpointError::InvalidTcpAddress(_))));
    }
}
