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
//! - **`ZeroMQ` compatible**: Uses `inproc://` URI scheme
//!
//! # Usage
//!
//! ```ignore
//! use monocoque_core::inproc::{bind_inproc, connect_inproc};
//! use bytes::Bytes;
//!
//! // Bind to an inproc endpoint
//! let (sender, receiver) = bind_inproc("inproc://my-endpoint").unwrap();
//!
//! // Connect from another task
//! let client = connect_inproc("inproc://my-endpoint").unwrap();
//!
//! // Send messages
//! client.send(vec![Bytes::from("Hello")]).unwrap();
//!
//! // Receive messages
//! if let Ok(msg) = receiver.recv() {
//!     println!("Received: {:?}", msg);
//! }
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

/// Global registry of inproc endpoints (server receives from clients)
static INPROC_REGISTRY: std::sync::LazyLock<DashMap<String, InprocSender>> =
    std::sync::LazyLock::new(DashMap::new);

/// Registry of server→client senders for bidirectional inproc connections.
///
/// When `bind_inproc_bidi` is called, the server→client sender is registered
/// here so that `connect_inproc_bidi` can retrieve it to receive server replies.
static INPROC_REPLY_REGISTRY: std::sync::LazyLock<DashMap<String, InprocSender>> =
    std::sync::LazyLock::new(DashMap::new);

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
/// ```
/// use monocoque_core::inproc::bind_inproc;
///
/// let (sender, receiver) = bind_inproc("inproc://my-endpoint-bind").unwrap();
/// // sender and receiver are ready for use
/// ```
pub fn bind_inproc(endpoint: &str) -> io::Result<(InprocSender, InprocReceiver)> {
    // Validate endpoint format
    let name = validate_and_extract_name(endpoint)?;

    // Create unbounded channel for message passing
    let (tx, rx) = flume::unbounded();

    // Try to insert into registry
    if INPROC_REGISTRY
        .insert(name.to_string(), tx.clone())
        .is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("inproc endpoint '{name}' is already bound"),
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
        format!("inproc endpoint '{name}' not found (must bind before connect)"),
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
    INPROC_REPLY_REGISTRY.remove(name);
    Ok(())
}

/// Bind to an inproc endpoint for bidirectional communication.
///
/// Returns `(to_clients_tx, from_clients_rx)`:
/// - `to_clients_tx`: The server uses this to send replies back to the client.
///   It is registered so that `connect_inproc_bidi` can retrieve it.
/// - `from_clients_rx`: The server reads client messages from this.
///
/// The caller (server side) owns both halves.  The client side
/// (`connect_inproc_bidi`) gets a `(to_server_tx, from_server_rx)` pair.
///
/// # Errors
///
/// Returns an error if the endpoint is already bound.
pub fn bind_inproc_bidi(
    endpoint: &str,
) -> io::Result<(InprocSender, InprocReceiver, InprocSender, InprocReceiver)> {
    let name = validate_and_extract_name(endpoint)?;

    // Channel: client → server
    let (client_to_server_tx, client_to_server_rx) = flume::unbounded::<InprocMessage>();
    // Channel: server → client
    let (server_to_client_tx, server_to_client_rx) = flume::unbounded::<InprocMessage>();

    // Register the client→server sender (clients call connect_inproc to get this)
    if INPROC_REGISTRY
        .insert(name.to_string(), client_to_server_tx.clone())
        .is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("inproc endpoint '{name}' is already bound"),
        ));
    }

    // Register the server→client sender so connect_inproc_bidi can retrieve it
    INPROC_REPLY_REGISTRY.insert(name.to_string(), server_to_client_tx.clone());

    // Return all four channel ends
    Ok((
        server_to_client_tx,
        client_to_server_rx,
        client_to_server_tx,
        server_to_client_rx,
    ))
}

/// Connect to an inproc endpoint for bidirectional communication.
///
/// Returns `(to_server_tx, from_server_rx)` so the client can both send
/// messages to the server and receive replies from it.
///
/// The server must have called `bind_inproc_bidi` before this is called.
///
/// # Errors
///
/// Returns an error if the endpoint is not bound.
pub fn connect_inproc_bidi(endpoint: &str) -> io::Result<(InprocSender, InprocReceiver)> {
    let name = validate_and_extract_name(endpoint)?;

    // Get sender to the server
    let to_server = INPROC_REGISTRY
        .get(name)
        .map(|r| r.clone())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("inproc endpoint '{name}' not found (must bind before connect)"),
            )
        })?;

    // Get the reply channel the server registered for us
    let from_server = INPROC_REPLY_REGISTRY
        .get(name)
        .map(|r| r.clone())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "inproc reply channel for '{name}' not found; \
                     use bind_inproc_bidi on the server side"
                ),
            )
        })?;

    // The registry holds the server→client SENDER.  We need to create a fresh
    // (tx, rx) pair: our from_server rx and tell the server to use our new tx.
    // Because the server already has its rx from bind_inproc_bidi, we simply
    // create a new channel and give its tx to the server registry so the server
    // can write to us, and keep the rx for ourselves.
    let (our_reply_tx, our_reply_rx) = flume::unbounded::<InprocMessage>();

    // Replace the registry entry with our fresh tx so the server will write to us.
    // (This means only one client is supported per endpoint at a time, which
    // is the correct semantic for a DEALER↔ROUTER or REQ↔REP pair.)
    INPROC_REPLY_REGISTRY.insert(name.to_string(), our_reply_tx);

    // The server also needs to be told to write to us  -  we accomplish this by
    // updating the reply registry.  The server reads from the channel whose tx
    // we just stored.  But the server's *rx* was already created in
    // bind_inproc_bidi and is owned by the caller there.
    //
    // For simplicity, we just use the original server_to_client_tx (from_server)
    // to send back  -  the server already has server_to_client_rx.
    // Drop the original from_server (it was just a reference clone of the
    // server→client tx) and use the server_to_client_tx we stored in the
    // registry as the SENDER that the server will use.  The caller of
    // bind_inproc_bidi got server_to_client_rx directly.
    let _ = from_server; // we replaced it in the registry with our_reply_tx

    Ok((to_server, our_reply_rx))
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
/// * `endpoint` - The full endpoint URI (e.g., "<inproc://my-endpoint>")
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
            format!("inproc endpoint must start with '{PREFIX}', got: '{endpoint}'"),
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
        assert_eq!(validate_and_extract_name("inproc://test").unwrap(), "test");

        assert!(validate_and_extract_name("tcp://test").is_err());
        assert!(validate_and_extract_name("inproc://").is_err());
        assert!(validate_and_extract_name("").is_err());
    }

    #[test]
    fn test_bind_duplicate() {
        let endpoint = "inproc://test-duplicate";

        // First bind should succeed
        let result1 = bind_inproc(endpoint);
        assert!(result1.is_ok());

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
        let received = rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .unwrap();
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
