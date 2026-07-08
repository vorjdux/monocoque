//! Runtime facade: the single place that names a concrete async runtime.
//!
//! Monocoque's whole socket stack is generic over the I/O traits from
//! `compio::io` (an owned-buffer, completion-style interface that maps cleanly
//! onto `io_uring`). Those traits, and the buffer types in `compio::buf`, are the
//! abstraction the rest of the code is written against, and they stay the same
//! no matter which runtime drives the sockets.
//!
//! What actually differs between runtimes is small: how you open a connection,
//! how you spawn a task, and how you arm a timer. This module collects exactly
//! those pieces behind one set of names so the rest of the crate (and the ZMTP
//! layer above it) never mentions `compio`, `tokio`, or `smol` directly.
//!
//! Each backend lives in its own file and is exposed under one shared name,
//! `backend`, selected by a Cargo feature. Only one is ever compiled:
//!
//! - `runtime-compio` (default): native `io_uring` through compio (see `compio.rs`).
//! - `runtime-tokio`: a thin adapter over tokio streams (see `tokio.rs`).
//! - `runtime-smol`: a thin adapter over smol's async-io streams (see `smol.rs`).
//!
//! The tokio and smol adapters implement the same `compio::io` traits by reading
//! straight into the owned buffer's memory, so there is no extra copy on the
//! data path. Exactly one of the three must be enabled.

#[cfg(all(feature = "runtime-compio", feature = "runtime-tokio"))]
compile_error!(
    "monocoque: enable exactly one runtime backend, not both \
     (`runtime-compio` or `runtime-tokio`)"
);

#[cfg(all(feature = "runtime-compio", feature = "runtime-smol"))]
compile_error!(
    "monocoque: enable exactly one runtime backend, not both \
     (`runtime-compio` or `runtime-smol`)"
);

#[cfg(all(feature = "runtime-tokio", feature = "runtime-smol"))]
compile_error!(
    "monocoque: enable exactly one runtime backend, not both \
     (`runtime-tokio` or `runtime-smol`)"
);

#[cfg(not(any(
    feature = "runtime-compio",
    feature = "runtime-tokio",
    feature = "runtime-smol"
)))]
compile_error!(
    "monocoque: no runtime backend selected; enable `runtime-compio` (default), \
     `runtime-tokio`, or `runtime-smol`"
);

// Exactly one backend file is compiled and re-exported under `backend`. The
// tokio/smol modules are additionally gated on the absence of higher-priority
// backends so that when several features are on at once, the `compile_error!`
// above is the message the user sees rather than a duplicate `backend`
// definition.

#[cfg(feature = "runtime-compio")]
#[path = "compio.rs"]
mod backend;

#[cfg(all(feature = "runtime-tokio", not(feature = "runtime-compio")))]
#[path = "tokio.rs"]
mod backend;

#[cfg(all(
    feature = "runtime-smol",
    not(feature = "runtime-compio"),
    not(feature = "runtime-tokio")
))]
#[path = "smol.rs"]
mod backend;

pub use backend::*;
