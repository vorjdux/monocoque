//! # Monocoque
//!
//! A high-performance, multi-protocol messaging runtime built on `io_uring`.
//!
//! ## Architecture
//!
//! Monocoque is structured as a **messaging kernel** with clean layering:
//!
//! - **`monocoque-core`**: Lock-free allocators, `io_uring` proactor, SPSC queues
//! - **Protocol crates**: Pure state machines (sans-IO)
//! - **`monocoque`**: Public API surface (this crate)
//!
//! ## Protocols (opt-in via features)
//!
//! Each protocol is gated behind a feature flag to avoid loading unused code:
//!
//! - **`zmq`** - `ZeroMQ` (ZMTP 3.x) implementation
//!
//! ```toml
//! [dependencies]
//! monocoque = { version = "0.1", features = ["zmq"] }
//! ```
//!
//! ## Quick Start
//!
//! ### `ZeroMQ` DEALER Socket (Client)
//!
//! ```rust,no_run
//! # #[cfg(feature = "zmq")]
//! use monocoque::zmq::prelude::*;
//!
//! # #[cfg(feature = "zmq")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to a ZeroMQ peer
//! let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
//!
//! // Send a multipart message
//! socket.send(vec![b"Hello".to_vec().into(), b"World".to_vec().into()]).await?;
//!
//! // Receive a reply
//! if let Some(msg) = socket.recv().await {
//!     println!("Received: {:?}", msg);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ### `ZeroMQ` ROUTER Socket (Server)
//!
//! ```rust,no_run
//! # #[cfg(feature = "zmq")]
//! use monocoque::zmq::prelude::*;
//!
//! # #[cfg(feature = "zmq")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Bind and accept first connection
//! let (listener, mut socket) = RouterSocket::bind("127.0.0.1:5555").await?;
//!
//! // Echo server
//! while let Some(msg) = socket.recv().await {
//!     socket.send(msg).await?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Performance
//!
//! - **Zero-copy**: Uses `bytes::Bytes` for refcounted message buffers
//! - **`io_uring`**: Native Linux async I/O (via `compio`)
//! - **Lock-free**: SPSC queues, no shared mutable state in hot paths
//! - **Sans-IO**: Protocol logic is pure, testable, and runtime-agnostic
//!
//! ## Safety
//!
//! - `unsafe` code is isolated to `monocoque-core/src/alloc/` (slab allocator)
//! - All protocol and routing layers are 100% safe Rust
//! - Formal invariants documented in `docs/blueprints/06-safety-model-and-unsafe-audit.md`

#![warn(missing_docs)]
#![warn(clippy::all)]
// Allow some pedantic patterns
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::future_not_send)] // Runtime-agnostic design
#![allow(clippy::missing_errors_doc)] // Will add gradually
#![allow(clippy::doc_markdown)] // Too many false positives

// Re-export core types
pub use bytes::Bytes;
pub use monocoque_core::options::SocketOptions;
pub use monocoque_core::reconnect::{ReconnectError, ReconnectState};
pub use monocoque_core::socket_type::SocketType;

// Protocol modules (opt-in via features)
#[cfg(feature = "zmq")]
pub mod zmq;

/// Development helpers (benches/tests)
pub mod dev_tracing;
