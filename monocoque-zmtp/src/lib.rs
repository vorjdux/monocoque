//! # Monocoque ZMTP
//!
//! High-performance `ZeroMQ` (ZMTP 3.1) protocol implementation in Rust.
//!
//! ## Overview
//!
//! Monocoque provides a clean, safe, and efficient implementation of `ZeroMQ` socket patterns:
//! - **DEALER**: Asynchronous request-reply with load balancing
//! - **ROUTER**: Server-side routing with identity-based addressing  
//! - **REQ**: Synchronous request-reply client (strict alternation)
//! - **REP**: Synchronous reply server (stateful envelope tracking)
//! - **PUB**: Publisher for broadcasting events
//! - **SUB**: Subscriber with topic-based filtering
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use monocoque_zmtp::DealerSocket;
//! use compio::net::TcpStream;
//! use bytes::Bytes;
//!
//! #[compio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let stream = TcpStream::connect("127.0.0.1:5555").await?;
//!     let socket = DealerSocket::new(stream).await;
//!     
//!     socket.send(vec![Bytes::from("Hello!")]).await?;
//!     let response = socket.recv().await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Features
//!
//! - **Zero-copy**: Messages use `Bytes` for efficient sharing
//! - **`io_uring`**: High-performance async I/O via `compio`
//! - **Sans-IO protocol**: Testable, runtime-agnostic design
//! - **Type-safe**: No unsafe code in protocol layer
//! - **Interoperable**: Compatible with libzmq

// Allow some pedantic lints
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::needless_pass_by_ref_mut)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::unused_async)]
#![allow(clippy::let_underscore_future)]

// Internal modules (not part of public API)
mod codec;
mod command;
mod greeting;
mod handshake;
pub mod integrated_actor; // Made public for integration tests
mod mechanism;
mod multipart;
mod utils;

// Public protocol types
pub mod session;

// Socket implementations
pub mod dealer;
pub mod publisher;
pub mod rep;
pub mod req;
pub mod router;
pub mod subscriber;

// Re-export socket types for clean API
pub use dealer::DealerSocket;
pub use publisher::PubSocket;
pub use rep::RepSocket;
pub use req::ReqSocket;
pub use router::RouterSocket;
pub use subscriber::SubSocket;

// Re-export commonly used types
pub use session::{SocketType, ZmtpSession};

/// Prelude module for convenient imports
///
/// ```rust
/// use monocoque_zmtp::prelude::*;
/// ```
pub mod prelude {
    pub use super::session::SocketType;
    pub use super::{DealerSocket, PubSocket, RepSocket, ReqSocket, RouterSocket, SubSocket};
    pub use bytes::Bytes;
}
