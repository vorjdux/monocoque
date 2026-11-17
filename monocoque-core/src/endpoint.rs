//! Endpoint abstraction for transport-agnostic socket addressing.
//!
//! Provides unified addressing for TCP and IPC transports with parsing support.
//!
//! Applications import these types from the `monocoque` crate, as
//! `monocoque::zmq::Endpoint` and `monocoque::zmq::EndpointError`. The
//! examples here name `monocoque_core` only because they are tested inside
//! this crate.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

/// Transport endpoint address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Endpoint {
    /// TCP transport: `tcp://host:port`
    Tcp(SocketAddr),
    /// IPC transport (Unix domain socket): `ipc:///path/to/socket`
    #[cfg(unix)]
    Ipc(PathBuf),
    /// In-process transport: `inproc://name`
    Inproc(String),
}

impl Endpoint {
    /// Parse an endpoint from a string.
    ///
    /// Supported formats:
    /// - `tcp://127.0.0.1:5555`
    /// - `tcp://[::1]:5555` (IPv6)
    /// - `tcp://*:5555` (every IPv4 interface) and `tcp://127.0.0.1:*` (a
    ///   port the OS picks), for binding
    /// - `tcp://localhost:5555` or any other host name. The name is looked up
    ///   once, when the endpoint is parsed, with the system resolver, which
    ///   blocks the calling thread for as long as the lookup takes. A
    ///   reconnect dials the address found then; it does not look the name up
    ///   again.
    /// - `ipc:///tmp/socket.sock` (Unix only)
    /// - `inproc://name`
    ///
    /// # Examples
    ///
    /// An application imports the type as `monocoque::zmq::Endpoint`; the
    /// path below is the one this crate's own tests use.
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
    ///
    /// let endpoint = Endpoint::parse("inproc://my-endpoint").unwrap();
    /// assert!(matches!(endpoint, Endpoint::Inproc(_)));
    /// ```
    pub fn parse(s: &str) -> Result<Self, EndpointError> {
        s.parse()
    }

    /// Returns true if this is a TCP endpoint.
    #[must_use]
    pub const fn is_tcp(&self) -> bool {
        matches!(self, Self::Tcp(_))
    }

    /// Returns true if this is an IPC endpoint.
    #[cfg(unix)]
    #[must_use]
    pub const fn is_ipc(&self) -> bool {
        matches!(self, Self::Ipc(_))
    }

    /// Returns true if this is an inproc endpoint.
    #[must_use]
    pub const fn is_inproc(&self) -> bool {
        matches!(self, Self::Inproc(_))
    }
}

impl FromStr for Endpoint {
    type Err = EndpointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(addr) = s.strip_prefix("tcp://") {
            resolve_tcp(addr)
                .map(Self::Tcp)
                .ok_or_else(|| EndpointError::InvalidTcpAddress(addr.to_string()))
        } else if let Some(path) = s.strip_prefix("ipc://") {
            #[cfg(unix)]
            {
                if !path.starts_with('/') {
                    return Err(EndpointError::InvalidScheme(s.to_string()));
                }
                Ok(Self::Ipc(PathBuf::from(path)))
            }
            #[cfg(not(unix))]
            {
                Err(EndpointError::IpcNotSupported)
            }
        } else if let Some(name) = s.strip_prefix("inproc://") {
            if name.is_empty() {
                Err(EndpointError::InvalidInprocName(
                    "inproc name cannot be empty".to_string(),
                ))
            } else if name.chars().any(char::is_control) {
                // Match the inproc transport's own validation
                // (inproc::validate_and_extract_name): reject NUL and other
                // control characters so a name cannot smuggle them past this
                // parser and diverge from what the transport accepts.
                Err(EndpointError::InvalidInprocName(
                    "inproc name cannot contain control characters".to_string(),
                ))
            } else {
                Ok(Self::Inproc(name.to_string()))
            }
        } else {
            Err(EndpointError::InvalidScheme(s.to_string()))
        }
    }
}

/// The address a `tcp://` endpoint names: an IP address and port, `*` for
/// every interface or for a port the OS picks, or a host name, which is
/// looked up here and now.
fn resolve_tcp(addr: &str) -> Option<SocketAddr> {
    use std::net::{Ipv4Addr, ToSocketAddrs};

    if let Ok(literal) = addr.parse() {
        return Some(literal);
    }
    let (host, port) = addr.rsplit_once(':')?;
    let port = if port == "*" { 0 } else { port.parse().ok()? };
    if host == "*" {
        return Some(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)));
    }
    if host.is_empty() {
        return None;
    }
    // A name can have several addresses. IPv4 first: a server bound to
    // `127.0.0.1` is not reachable through the `::1` that `localhost` may
    // list ahead of it.
    let mut found: Vec<SocketAddr> = (host, port).to_socket_addrs().ok()?.collect();
    found.sort_by_key(SocketAddr::is_ipv6);
    found.into_iter().next()
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "tcp://{addr}"),
            #[cfg(unix)]
            Self::Ipc(path) => write!(f, "ipc://{}", path.display()),
            Self::Inproc(name) => write!(f, "inproc://{name}"),
        }
    }
}

/// Errors that can occur when parsing or using endpoints.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EndpointError {
    #[error("Invalid scheme in endpoint: {0} (expected tcp://, ipc://, or inproc://)")]
    InvalidScheme(String),

    #[error("Invalid TCP address: {0}")]
    InvalidTcpAddress(String),

    #[error("Invalid inproc name: {0}")]
    InvalidInprocName(String),

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
    fn test_parse_tcp_wildcards() {
        assert_eq!(
            Endpoint::parse("tcp://*:5555").unwrap().to_string(),
            "tcp://0.0.0.0:5555"
        );
        assert_eq!(
            Endpoint::parse("tcp://127.0.0.1:*").unwrap().to_string(),
            "tcp://127.0.0.1:0"
        );
        assert_eq!(
            Endpoint::parse("tcp://*:*").unwrap().to_string(),
            "tcp://0.0.0.0:0"
        );
    }

    #[test]
    fn test_parse_tcp_host_name() {
        // `localhost` resolves without a network. IPv4 is preferred when the
        // name has both.
        let Endpoint::Tcp(addr) = Endpoint::parse("tcp://localhost:5555").unwrap() else {
            panic!("not a TCP endpoint");
        };
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), 5555);

        for bad in [
            "tcp://localhost",
            "tcp://:5555",
            "tcp://localhost:port",
            "tcp://localhost:70000",
            "tcp://no-such-host.invalid:5555",
        ] {
            assert!(
                matches!(
                    Endpoint::parse(bad),
                    Err(EndpointError::InvalidTcpAddress(_))
                ),
                "{bad} should not parse"
            );
        }
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

    #[cfg(unix)]
    #[test]
    fn test_invalid_ipc_relative_path() {
        let result = Endpoint::parse("ipc://relative.sock");
        assert!(matches!(result, Err(EndpointError::InvalidScheme(_))));
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

    #[test]
    fn test_parse_inproc() {
        let endpoint = Endpoint::parse("inproc://my-endpoint").unwrap();
        assert!(matches!(endpoint, Endpoint::Inproc(_)));
        assert_eq!(endpoint.to_string(), "inproc://my-endpoint");
    }

    #[test]
    fn test_invalid_inproc_empty() {
        let result = Endpoint::parse("inproc://");
        assert!(matches!(result, Err(EndpointError::InvalidInprocName(_))));
    }

    #[test]
    fn test_invalid_inproc_control_characters() {
        // Must reject the same inputs the inproc transport rejects, including an
        // embedded NUL, so the two cannot diverge.
        for bad in ["inproc://tenant\0shadow", "inproc://a\tb", "inproc://x\ny"] {
            assert!(
                matches!(
                    Endpoint::parse(bad),
                    Err(EndpointError::InvalidInprocName(_))
                ),
                "control character in {bad:?} should be rejected"
            );
        }
    }
}
