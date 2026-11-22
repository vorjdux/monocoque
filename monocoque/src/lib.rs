//! # Monocoque
//!
//! A `ZeroMQ`-compatible messaging library in Rust. It implements ZMTP and the
//! `ZeroMQ` socket types, and interoperates with libzmq peers.
//!
//! ## Architecture
//!
//! Every socket owns one multiplexer that serves all of its peers, on the
//! runtime thread that created the socket. A socket binds and connects any
//! number of `tcp://`, `ipc://` and `inproc://` endpoints, and sockets are
//! `!Send`: for more cores, run one runtime per thread.
//!
//! - **`monocoque-core`**: the socket multiplexer, the `Msg` type, transports,
//!   backpressure, and the runtime facade ([`rt`])
//! - **`monocoque-zmtp`**: the ZMTP protocol (handshake, NULL/PLAIN/CURVE,
//!   framing) and the socket types built on the multiplexer
//! - **`monocoque`**: the public API (this crate)
//!
//! ## Features
//!
//! The socket types live in `monocoque::zmq`, behind the `zmq` feature, which
//! is not on by default:
//!
//! ```toml
//! [dependencies]
//! monocoque-rs = { version = "1.0", features = ["zmq"] }
//! ```
//!
//! One of three runtime backends is selected at compile time: `runtime-compio`
//! (`io_uring`, the default), `runtime-tokio` or `runtime-smol`. They are
//! mutually exclusive, so the last two need `default-features = false`. The
//! `draft` feature adds the draft socket types under `zmq::draft`.
//!
//! ## Quick Start
//!
//! Start the runtime with [`rt::LocalRuntime`], which builds the right
//! single-threaded runtime for the enabled backend:
//!
//! ```rust,no_run
//! # #[cfg(feature = "zmq")]
//! # fn main() -> monocoque::zmq::Result<()> {
//! monocoque::rt::LocalRuntime::new()?.block_on(async {
//!     let mut socket = monocoque::zmq::DealerSocket::new();
//!     socket.connect("tcp://127.0.0.1:5555").await?;
//!     socket.send("hello").await?;
//!     Ok(())
//! })
//! # }
//! # #[cfg(not(feature = "zmq"))]
//! # fn main() {}
//! ```
//!
//! ### `ZeroMQ` DEALER Socket (Client)
//!
//! ```rust,no_run
//! # #[cfg(feature = "zmq")]
//! use monocoque::zmq::prelude::*;
//!
//! # #[cfg(feature = "zmq")]
//! # async fn example() -> Result<()> {
//! // Connect to a ZeroMQ peer; the connection is made in the background
//! let mut socket = DealerSocket::new();
//! socket.connect("tcp://127.0.0.1:5555").await?;
//!
//! // Send a multipart message
//! let frames = [Bytes::from_static(b"Hello"), Bytes::from_static(b"World")];
//! socket.send(Msg::from_iter(frames)).await?;
//!
//! // Receive a reply, borrowed from the socket without a copy
//! let msg = socket.recv().await?;
//! println!("Received {} frames", msg.len());
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
//! # async fn example() -> Result<()> {
//! // Bind; any number of peers can connect
//! let mut socket = RouterSocket::new();
//! socket.bind("tcp://127.0.0.1:5555").await?;
//!
//! // Echo server: the first frame is the sender's identity, so sending the
//! // message back routes it to that peer
//! loop {
//!     let msg = socket.recv_owned().await?;
//!     socket.send(msg).await?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Performance
//!
//! - **Refcounted frames**: frames are `bytes::Bytes`, so passing a message on
//!   copies no payload
//! - **Borrowed receive**: `recv()` lends a `MsgRef` into a slot the socket
//!   reuses, so a receive loop allocates nothing; `recv_owned()` keeps the message
//! - **One thread per socket**: a socket and all of its peers run on the
//!   thread that owns it, and the I/O is done inline on that thread
//! - **Write coalescing**: opt-in batching of small messages into one write
//!   (`SocketOptions::with_write_coalescing`)
//!
//! ## Safety
//!
//! This crate and `monocoque-zmtp` contain no `unsafe`. The `unsafe` in
//! `monocoque-core` is confined to two modules, an owned-buffer read helper
//! (`io`) and raw-fd socket calls (`tcp`), and is inventoried in the README's
//! Safety section.

#![warn(missing_docs)]

// Re-export core types
pub use bytes::Bytes;
pub use monocoque_core::options::SocketOptions;
pub use monocoque_core::reconnect::{ReconnectError, ReconnectState};

/// Runtime-agnostic networking types (TCP/Unix streams, listeners).
///
/// These resolve to the active backend (compio by default, tokio with the
/// `runtime-tokio` feature, smol with the `runtime-smol` feature). `Socket::attach` accepts the
/// streams re-exported here, so application code never names a runtime crate
/// directly.
pub use monocoque_core::rt;

// Protocol modules (opt-in via features)
#[cfg(feature = "zmq")]
pub mod zmq;
