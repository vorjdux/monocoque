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
//!
//! ## For Application Development
//!
//! ```toml
//! [dependencies]
//! monocoque = { version = "0.1", features = ["zmq"] }
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
#![allow(clippy::future_not_send)] // Runtime-agnostic design
#![allow(clippy::uninlined_format_args)] // Style preference
#![allow(clippy::missing_errors_doc)] // Will add gradually
#![allow(clippy::doc_markdown)] // Too many false positives
#![allow(clippy::while_let_loop)] // Sometimes clearer as explicit loop
#![allow(clippy::option_if_let_else)] // Sometimes clearer as if/else
#![allow(clippy::never_loop)] // State machines use loop with early returns

// Internal modules (not part of public API)
mod codec;
mod greeting;
mod handshake;
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
pub use monocoque_core::config::BufferConfig;
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
