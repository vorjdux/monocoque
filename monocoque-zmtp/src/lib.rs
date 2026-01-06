//! # Monocoque ZMTP
//!
//! High-performance ZeroMQ (ZMTP 3.1) protocol implementation in Rust.
//!
//! ## Overview
//!
//! Monocoque provides a clean, safe, and efficient implementation of ZeroMQ socket patterns:
//! - **DEALER**: Asynchronous request-reply with load balancing
//! - **ROUTER**: Server-side routing with identity-based addressing  
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
//! - **io_uring**: High-performance async I/O via `compio`
//! - **Sans-IO protocol**: Testable, runtime-agnostic design
//! - **Type-safe**: No unsafe code in protocol layer
//! - **Interoperable**: Compatible with libzmq

// Internal modules (not part of public API)
mod codec;
mod command;
mod greeting;
pub mod integrated_actor; // Made public for integration tests
mod mechanism;
mod multipart;
mod utils;

// Public protocol types
pub mod session;

// Socket implementations
pub mod dealer;
pub mod publisher;
pub mod router;
pub mod subscriber;

// Re-export socket types for clean API
pub use dealer::DealerSocket;
pub use publisher::PubSocket;
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
    pub use super::{DealerSocket, PubSocket, RouterSocket, SubSocket};
    pub use bytes::Bytes;
}
