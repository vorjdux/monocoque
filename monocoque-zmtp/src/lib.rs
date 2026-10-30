//! # Monocoque ZMTP
//!
//! **Internal protocol implementation crate for Monocoque.**
//!
//! ⚠️ **This is an internal implementation detail. Use the `monocoque` crate for the public API.**
//!
//! This crate provides the ZMTP protocol (greeting, handshake, NULL/PLAIN/CURVE
//! security, framing) and the ZeroMQ socket types built on the socket
//! multiplexer in `monocoque-core`. It speaks ZMTP 3.1 and 3.0, and falls back
//! to ZMTP 1.0 and 2.0 framing for legacy peers. For application development,
//! use `monocoque::zmq::*`.
//!
//! ## Socket Types (Internal API)
//!
//! - **DEALER**: Asynchronous request-reply with load balancing
//! - **ROUTER**: Server-side routing with identity-based addressing
//! - **REQ**: Synchronous request-reply client (strict alternation)
//! - **REP**: Synchronous reply server (stateful envelope tracking)
//! - **PUB** / **XPUB**: Publisher for broadcasting events
//! - **SUB** / **XSUB**: Subscriber with topic-based filtering
//! - **PUSH**: Pipeline push for task distribution
//! - **PULL**: Pipeline pull for task reception
//! - **PAIR**: Exclusive peer-to-peer communication
//! - **STREAM**: Raw TCP peers
//!
//! With the `draft` feature: **CLIENT** / **SERVER**, **RADIO** / **DISH**,
//! **SCATTER** / **GATHER** and **CHANNEL**. These need ZMTP 3.1 and carry
//! single-part messages.
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
//! async fn main() -> monocoque::zmq::Result<()> {
//!     let mut socket = DealerSocket::new();
//!     socket.connect("tcp://127.0.0.1:5555").await?;
//!     socket.send("Hello!").await?;
//!     let response = socket.recv_owned().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Features
//!
//! - **Zero-copy**: Messages use `Bytes` for efficient sharing
//! - **Runtime backends**: high-performance async I/O via `compio` (io_uring), tokio, or smol
//! - **Interoperable**: Compatible with libzmq

pub mod error;
mod greeting;
mod handshake;
pub mod sock;
mod utils;
pub mod wire;

pub mod session;

// Security mechanisms
pub mod security;

pub use session::SocketType;
pub use sock::Socket;
pub use sock::kinds::{
    DealerSocket, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket,
    RouterSocket, SubSocket, XPubSocket, XSubSocket,
};
pub use sock::stream::StreamSocket;

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
    pub use super::{
        DealerSocket, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket,
        RouterSocket, Socket, StreamSocket, SubSocket, XPubSocket, XSubSocket,
    };
    pub use bytes::Bytes;
}
