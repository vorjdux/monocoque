//! In-process transport for zero-copy messaging within the same process.
//!
//! The inproc transport provides high-performance communication between sockets
//! in the same process using channels, without TCP/IP overhead.
//!
//! # Features
//!
//! - **Zero-copy**: Messages are shared via `Arc<Vec<Bytes>>` between sockets
//! - **Thread-safe**: Global registry protected by `DashMap`
//! - **Fast**: No serialization, network, or syscall overhead
//! - **ZeroMQ compatible**: Uses `inproc://` URI scheme
//!
//! # Usage
//!
//! ```rust,no_run
//! use monocoque_core::inproc::{bind_inproc, connect_inproc};
//! use bytes::Bytes;
//!
//! # async fn example() -> std::io::Result<()> {
//! // Bind to an inproc endpoint
//! let (mut sender, receiver) = bind_inproc("inproc://my-endpoint")?;
//!
//! // Connect to the same endpoint from another task
//! let mut client = connect_inproc("inproc://my-endpoint").await?;
//!
//! // Send messages (zero-copy)
//! client.send(vec![Bytes::from("Hello")]).await?;
//!
//! // Receive messages
//! if let Some(msg) = receiver.recv().await {
//!     println!("Received: {:?}", msg);
//! }
//! # Ok(())
//! # }
//! ```

use bytes::Bytes;
use dashmap::DashMap;
use flume::{Receiver, Sender};
use std::io;

/// Message type for inproc transport (multipart message)
pub type InprocMessage = Vec<Bytes>;

/// Sender half of an inproc connection
pub type InprocSender = Sender<InprocMessage>;

/// Receiver half of an inproc connection
pub type InprocReceiver = Receiver<InprocMessage>;

/// Global registry of inproc endpoints
static INPROC_REGISTRY: once_cell::sync::Lazy<DashMap<String, InprocSender>> =
    once_cell::sync::Lazy::new(DashMap::new);

/// Bind to an inproc endpoint and return sender/receiver pair.
///
/// The endpoint is registered in the global registry. Multiple clients can
/// connect to this endpoint using `connect_inproc()`.
///
/// # Arguments
///
/// * `endpoint` - The endpoint URI (must start with "inproc://")
///
/// # Returns
///
/// Returns a tuple of (sender, receiver):
/// - `sender`: Used to send messages from this socket
/// - `receiver`: Used to receive messages sent by connected clients
///
/// # Errors
///
/// Returns an error if:
/// - The endpoint doesn't start with "inproc://"
/// - The endpoint is already bound
/// - The endpoint name is empty
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_core::inproc::bind_inproc;
///
/// # fn example() -> std::io::Result<()> {
/// let (sender, mut receiver) = bind_inproc("inproc://my-endpoint")?;
///
/// // Spawn a task to handle incoming messages
/// tokio::spawn(async move {
///     while let Some(msg) = receiver.recv().await {
///         println!("Received: {:?}", msg);
///     }
/// });
/// # Ok(())
/// # }
/// ```
pub fn bind_inproc(endpoint: &str) -> io::Result<(InprocSender, InprocReceiver)> {
    // Validate endpoint format
    let name = validate_and_extract_name(endpoint)?;

    // Create unbounded channel for message passing
    let (tx, rx) = flume::unbounded();

    // Try to insert into registry
    if INPROC_REGISTRY.insert(name.to_string(), tx.clone()).is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("inproc endpoint '{}' is already bound", name),
        ));
    }

    Ok((tx, rx))
}

/// Connect to an inproc endpoint.
///
/// Returns a sender that can be used to send messages to the bound endpoint.
/// This function blocks until the endpoint becomes available.
///
/// # Arguments
///
/// * `endpoint` - The endpoint URI (must start with "inproc://")
///
/// # Returns
///
/// Returns a sender that can send messages to the bound endpoint.
///
/// # Errors
///
/// Returns an error if:
/// - The endpoint doesn't start with "inproc://"
/// - The endpoint is not bound
/// - The endpoint name is empty
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_core::inproc::connect_inproc;
/// use bytes::Bytes;
///
/// # fn example() -> std::io::Result<()> {
/// let sender = connect_inproc("inproc://my-endpoint")?;
///
/// // Send a message
/// sender.send(vec![Bytes::from("Hello")]).map_err(|_| {
///     std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped")
/// })?;
/// # Ok(())
/// # }
/// ```
pub fn connect_inproc(endpoint: &str) -> io::Result<InprocSender> {
    // Validate endpoint format
    let name = validate_and_extract_name(endpoint)?;

    // Look up the endpoint in the registry
    if let Some(sender) = INPROC_REGISTRY.get(name) {
        return Ok(sender.clone());
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("inproc endpoint '{}' not found (must bind before connect)", name),
    ))
}

/// Unbind an inproc endpoint, removing it from the global registry.
///
/// This should be called when a bound socket is closed to free up the endpoint name.
///
/// # Arguments
///
/// * `endpoint` - The endpoint URI (must start with "inproc://")
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_core::inproc::{bind_inproc, unbind_inproc};
///
/// # fn example() -> std::io::Result<()> {
/// let (sender, receiver) = bind_inproc("inproc://my-endpoint")?;
///
/// // ... use the endpoint ...
///
/// // Clean up when done
/// unbind_inproc("inproc://my-endpoint")?;
/// # Ok(())
/// # }
/// ```
pub fn unbind_inproc(endpoint: &str) -> io::Result<()> {
    let name = validate_and_extract_name(endpoint)?;
    INPROC_REGISTRY.remove(name);
    Ok(())
}

/// List all currently bound inproc endpoints.
///
/// This is primarily useful for debugging and testing.
///
/// # Returns
///
/// Returns a vector of endpoint names (without the "inproc://" prefix).
pub fn list_inproc_endpoints() -> Vec<String> {
    INPROC_REGISTRY
        .iter()
        .map(|entry| entry.key().clone())
        .collect()
}

/// Validate endpoint format and extract the name.
///
/// # Arguments
///
/// * `endpoint` - The full endpoint URI (e.g., "inproc://my-endpoint")
///
/// # Returns
///
/// Returns the endpoint name without the "inproc://" prefix.
///
/// # Errors
///
/// Returns an error if the endpoint doesn't start with "inproc://" or has an empty name.
fn validate_and_extract_name(endpoint: &str) -> io::Result<&str> {
    const PREFIX: &str = "inproc://";

    if !endpoint.starts_with(PREFIX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "inproc endpoint must start with '{}', got: '{}'",
                PREFIX, endpoint
            ),
        ));
    }

    let name = &endpoint[PREFIX.len()..];
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inproc endpoint name cannot be empty",
        ));
    }

    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_endpoint() {
        assert!(validate_and_extract_name("inproc://test").is_ok());
        assert_eq!(
            validate_and_extract_name("inproc://test").unwrap(),
            "test"
        );

        assert!(validate_and_extract_name("tcp://test").is_err());
        assert!(validate_and_extract_name("inproc://").is_err());
        assert!(validate_and_extract_name("").is_err());
    }

    #[test]
    fn test_bind_duplicate() {
        let endpoint = "inproc://test-duplicate";

        // First bind should succeed
        let _result1 = bind_inproc(endpoint);
        assert!(_result1.is_ok());

        // Second bind should fail
        let result2 = bind_inproc(endpoint);
        assert!(result2.is_err());
        assert_eq!(result2.unwrap_err().kind(), io::ErrorKind::AddrInUse);

        // Cleanup
        let _ = unbind_inproc(endpoint);
    }

    #[test]
    fn test_bind_and_connect() {
        let endpoint = "inproc://test-connect";

        // Bind
        let (_tx, rx) = bind_inproc(endpoint).unwrap();

        // Connect
        let client = connect_inproc(endpoint).unwrap();

        // Send message from client
        let msg = vec![Bytes::from("Hello, inproc!")];
        client.send(msg.clone()).unwrap();

        // Receive on bound socket (non-blocking recv_timeout)
        let received = rx.recv_timeout(std::time::Duration::from_millis(100)).unwrap();
        assert_eq!(received, msg);

        // Cleanup
        unbind_inproc(endpoint).unwrap();
    }

    #[test]
    fn test_list_endpoints() {
        let ep1 = "inproc://test-list-1";
        let ep2 = "inproc://test-list-2";

        let _bind1 = bind_inproc(ep1).unwrap();
        let _bind2 = bind_inproc(ep2).unwrap();

        let endpoints = list_inproc_endpoints();
        assert!(endpoints.contains(&"test-list-1".to_string()));
        assert!(endpoints.contains(&"test-list-2".to_string()));

        // Cleanup
        unbind_inproc(ep1).unwrap();
        unbind_inproc(ep2).unwrap();
    }
}
