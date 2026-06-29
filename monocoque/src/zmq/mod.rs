//! ZeroMQ protocol implementation.
//!
//! This module provides high-performance ZeroMQ-compatible sockets built on `io_uring`.
//!
//! # Socket Types
//!
//! - [`DealerSocket`] - Asynchronous request-reply client (load-balanced)
//! - [`RouterSocket`] - Identity-based routing server
//! - [`ReqSocket`] - Synchronous request-reply client (strict alternation)
//! - [`RepSocket`] - Synchronous reply server (stateful envelope tracking)
//! - [`PubSocket`] - Publisher (broadcast to subscribers)
//! - [`SubSocket`] - Subscriber (receive filtered messages)
//! - [`PushSocket`] - Pipeline push (distribute tasks)
//! - [`PullSocket`] - Pipeline pull (receive tasks)
//! - [`PushFanOut`] - Ventilator that round-robins tasks across a pool of PULL workers
//! - [`PullFanIn`] - Sink that merges results from a pool of PUSH workers
//! - [`PairSocket`] - Exclusive pair connection
//! - [`XPubSocket`] - Extended publisher (subscription events)
//! - [`XSubSocket`] - Extended subscriber (subscription forwarding)
//! - [`StreamSocket`] - Raw TCP bridging (no ZMTP handshake)
//!
//! # Features
//!
//! - **Endpoint Parsing**: Use `Endpoint::parse("tcp://...")` or `Endpoint::parse("ipc://...")`
//! - **Socket Monitoring**: Subscribe to connection events via `socket.monitor()`
//! - **IPC Transport**: Unix domain sockets for low-latency local communication (Unix only)
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
//! if let Ok(Some(reply)) = socket.recv().await {
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
//! while let Ok(Some(msg)) = socket.recv().await {
//!     socket.send(msg).await?; // Echo back
//! }
//! # Ok(())
//! # }
//! ```

mod common;
mod dealer;
mod publisher;
mod pull;
mod pull_fanin;
mod push;
mod push_fanout;
mod rep;
mod req;
mod router;
mod subscriber;

// Re-export socket types
pub use dealer::DealerSocket;
pub use monocoque_core::config::BufferConfig;
pub use monocoque_core::endpoint::{Endpoint, EndpointError};
pub use monocoque_core::monitor::{SocketEvent, SocketMonitor};
pub use monocoque_core::options::SocketOptions;
pub use monocoque_core::socket_type::SocketType;
pub use monocoque_core::subscription::{Subscription, SubscriptionEvent, SubscriptionTrie};
pub use monocoque_zmtp::proxy;
pub use monocoque_zmtp::{PairSocket, StreamSocket, XPubSocket, XSubSocket};
pub use publisher::PubSocket;
pub use pull::PullSocket;
pub use pull_fanin::PullFanIn;
pub use push::PushSocket;
pub use push_fanout::PushFanOut;
pub use rep::RepSocket;
pub use req::ReqSocket;
pub use router::RouterSocket;
pub use subscriber::SubSocket;

#[cfg(unix)]
pub use monocoque_core::ipc;

/// Convenient imports for ZeroMQ protocol.
///
/// # Example
///
/// ```rust
/// use monocoque::zmq::prelude::*;
///
/// // Now you have:
/// // - DealerSocket, RouterSocket, ReqSocket, RepSocket
/// // - PubSocket, SubSocket, XPubSocket, XSubSocket
/// // - PushSocket, PullSocket, PushFanOut, PullFanIn, PairSocket
/// // - Bytes for zero-copy messages
/// // - BufferConfig, SocketOptions, SocketType for configuration
/// ```
pub mod prelude {
    pub use super::proxy::{ProxyCommand, ProxySocket, proxy, proxy_steerable};
    pub use super::{
        BufferConfig, DealerSocket, PairSocket, PubSocket, PullFanIn, PullSocket, PushFanOut,
        PushSocket, RepSocket, ReqSocket, RouterSocket, SocketOptions, StreamSocket, SubSocket,
        Subscription, SubscriptionEvent, SubscriptionTrie, XPubSocket, XSubSocket,
    };
    pub use bytes::Bytes;
    pub use monocoque_core::socket_type::SocketType;
}
