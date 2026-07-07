//! # Monocoque
//!
//! A high-performance, multi-protocol messaging runtime. It runs on `io_uring`
//! (via `compio`) by default, with optional tokio and smol backends for
//! portability.
//!
//! ## Architecture
//!
//! Monocoque is structured as a **messaging kernel** with clean layering:
//!
//! - **`monocoque-core`**: Lock-free allocators, runtime facade, SPSC queues
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
//! monocoque-rs = { version = "0.1", features = ["zmq"] }
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
//! if let Ok(Some(msg)) = socket.recv().await {
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
//! while let Ok(Some(msg)) = socket.recv().await {
//!     socket.send(msg).await?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Performance
//!
//! - **Zero-copy**: Uses `bytes::Bytes` for refcounted message buffers
//! - **Runtime backends**: native `io_uring` via `compio` (default), or tokio
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
#![allow(clippy::return_self_not_must_use)] // Builder patterns are obvious
#![allow(clippy::missing_panics_doc)] // Most panics are unreachable
#![allow(clippy::missing_const_for_fn)] // Not always an optimization
#![allow(clippy::multiple_crate_versions)] // Transitive dependencies
#![allow(clippy::doc_lazy_continuation)] // Doc formatting is intentional
#![allow(clippy::manual_let_else)] // Match expressions sometimes clearer
#![allow(clippy::empty_line_after_outer_attr)] // Spacing is intentional

// Re-export core types
pub use bytes::Bytes;
pub use monocoque_core::options::SocketOptions;
pub use monocoque_core::reconnect::{ReconnectError, ReconnectState};
pub use monocoque_core::socket_type::SocketType;

/// Runtime-agnostic networking types (TCP/Unix streams, listeners).
///
/// These resolve to the active backend (compio by default, tokio with the
/// `runtime-tokio` feature). Socket constructors such as `from_unix_stream`
/// accept the types re-exported here, so application code never names a runtime
/// crate directly.
pub use monocoque_core::rt;

// Protocol modules (opt-in via features)
#[cfg(feature = "zmq")]
pub mod zmq;

/// Development helpers (benches/tests)
#[doc(hidden)]
pub mod dev_tracing;
