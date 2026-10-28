//! # Monocoque ZMTP
//!
//! **Internal protocol implementation crate for Monocoque.**
//!
//! ⚠️ **This is an internal implementation detail. Use the `monocoque` crate for the public API.**
//!
//! This crate provides the low-level ZMTP 3.1 protocol implementation with direct stream I/O.
//! For application development, use `monocoque::zmq::*` which provides a higher-level, more
//! ergonomic API with proper error handling and convenience methods.
//!
//! ## Socket Types (Internal API)
//!
//! - **DEALER**: Asynchronous request-reply with load balancing
//! - **ROUTER**: Server-side routing with identity-based addressing  
//! - **REQ**: Synchronous request-reply client (strict alternation)
//! - **REP**: Synchronous reply server (stateful envelope tracking)
//! - **PUB**: Publisher for broadcasting events
//! - **SUB**: Subscriber with topic-based filtering
//! - **PUSH**: Pipeline push for task distribution
//! - **PULL**: Pipeline pull for task reception
//! - **PAIR**: Exclusive peer-to-peer communication
//!
//! ## For Application Development
//!
//! ```toml
//! [dependencies]
//! monocoque-rs = { version = "0.4", features = ["zmq"] }
//! ```
//!
//! ```rust,ignore
//! use monocoque::zmq::DealerSocket;
//!
//! #[compio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
//!     socket.send(vec![b"Hello!".into()]).await?;
//!     let response = socket.recv().await;
//!     Ok(())
//! }
//! ```
//!
//! ## Features
//!
//! - **Zero-copy**: Messages use `Bytes` for efficient sharing
//! - **Runtime backends**: high-performance async I/O via `compio` (io_uring), tokio, or smol
//! - **Sans-IO protocol**: Testable, runtime-agnostic design
//! - **Interoperable**: Compatible with libzmq

// Internal modules (not part of public API)
pub(crate) mod base;
pub mod codec;
mod greeting;
mod handshake;
mod inproc_stream;
mod utils;

// Public protocol types
/// Sans-IO ZMTP session and related types.
pub mod session;
pub mod socket_trait;

// Security mechanisms
pub mod security;

// Socket implementations
pub mod dealer;
pub mod pair;
pub mod proxy;
/// PUB socket implementation.
pub mod publisher;
pub mod pull;
pub mod push;
pub mod rep;
pub mod req;
pub mod router;
pub mod stream;
pub mod subscriber;
pub mod xpub;
pub mod xsub;

// Re-export socket types for clean API
pub use dealer::DealerSocket;
pub use pair::PairSocket;
pub use publisher::PubSocket;
pub use pull::PullSocket;
pub use push::PushSocket;
pub use rep::RepSocket;
pub use req::ReqSocket;
pub use router::RouterSocket;
pub use stream::StreamSocket;
pub use subscriber::SubSocket;
pub use xpub::XPubSocket;
pub use xsub::XSubSocket;

// Re-export commonly used types
pub use session::{SocketType, ZmtpSession};
pub use socket_trait::Socket;

// Wire-parser entry points reachable from the fuzz crate (monocoque-fuzz).
// This is an internal implementation crate, so exposing the greeting and READY
// command parsers here widens no public-facing (monocoque) API surface.
pub use greeting::ZmtpGreeting;
pub use handshake::parse_ready_command;

/// Prelude module for convenient imports
///
/// ```rust
/// use monocoque_zmtp::prelude::*;
/// ```
pub mod prelude {
    pub use super::session::SocketType;
    pub use super::socket_trait::Socket;
    pub use super::{
        DealerSocket, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket,
        RouterSocket, SubSocket, XPubSocket, XSubSocket,
    };
    pub use bytes::Bytes;
}
