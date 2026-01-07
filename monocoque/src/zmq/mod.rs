//! ZeroMQ protocol implementation.
//!
//! This module provides high-performance ZeroMQ-compatible sockets built on `io_uring`.
//!
//! # Socket Types
//!
//! - [`DealerSocket`] - Asynchronous request-reply client (load-balanced)
//! - [`RouterSocket`] - Identity-based routing server
//! - [`PubSocket`] - Publisher (broadcast to subscribers)
//! - [`SubSocket`] - Subscriber (receive filtered messages)
//!
//! # Quick Start
//!
//! ## DEALER (Client)
//!
//! ```rust,no_run
//! use monocoque::zmq::DealerSocket;
//! use bytes::Bytes;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
//! socket.send(vec![Bytes::from("REQUEST")]).await?;
//!
//! if let Some(reply) = socket.recv().await {
//!     println!("Got reply: {:?}", reply);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## ROUTER (Server)
//!
//! ```rust,no_run
//! use monocoque::zmq::RouterSocket;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let (listener, mut socket) = RouterSocket::bind("127.0.0.1:5555").await?;
//!
//! while let Some(msg) = socket.recv().await {
//!     socket.send(msg).await?; // Echo back
//! }
//! # Ok(())
//! # }
//! ```

mod common;
mod dealer;
mod publisher;
mod router;
mod subscriber;

// Re-export socket types
pub use dealer::DealerSocket;
pub use publisher::PubSocket;
pub use router::RouterSocket;
pub use subscriber::SubSocket;


/// Convenient imports for ZeroMQ protocol.
///
/// # Example
///
/// ```rust
/// use monocoque::zmq::prelude::*;
///
/// // Now you have:
/// // - DealerSocket, RouterSocket, PubSocket, SubSocket
/// // - Bytes for zero-copy messages
/// ```
pub mod prelude {
    pub use super::{DealerSocket, PubSocket, RouterSocket, SubSocket};
    pub use bytes::Bytes;
}
