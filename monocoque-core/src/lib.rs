//! Monocoque Core
//!
//! This crate contains the runtime-agnostic core building blocks:
//! - Zero-copy segmented buffer (`buffer`)
//! - Shared owned-buffer I/O helpers for the runtime backends (`io`)
//! - TCP utilities for high-performance networking (`tcp`)
//! - PUB/SUB subscription index (`pubsub`)
//! - Byte-based backpressure (`backpressure`)
//! - Error types (`error`)

// The tcp module needs raw fd/socket access for socket configuration
#![cfg_attr(not(test), deny(unsafe_code))]

pub mod backpressure;
pub mod buffer;
pub mod config;
pub mod endpoint;
pub mod error;
pub mod inproc;
pub mod io;
pub mod localq;
pub mod message;
pub mod message_builder;
pub mod monitor;
pub mod msg;
pub mod options;
pub mod poison;
pub mod reconnect;
pub mod rt;
pub mod socket_type;
pub mod subscription;
pub mod tcp;
pub mod timeout;
pub mod transport;

#[cfg(unix)]
pub mod ipc;

pub mod pubsub {
    pub mod index;
}

// Optional: a small prelude to make downstream crates ergonomic.
// Keep it minimal to avoid API lock-in.
pub mod prelude {
    pub use crate::backpressure::{BytePermits, NoOpPermits, Permit, SemaphorePermits};
    pub use crate::buffer::SegmentedBuffer;
    pub use crate::endpoint::Endpoint;
    pub use crate::message_builder::Message;
    pub use crate::monitor::{SocketEvent, SocketMonitor};
    pub use crate::options::SocketOptions;
    pub use crate::poison::PoisonGuard;
    pub use crate::pubsub::index::{PeerKey, SubscriptionIndex};
    pub use crate::reconnect::{ReconnectError, ReconnectState};
    pub use crate::socket_type::SocketType;
    pub use crate::tcp::{configure_tcp_keepalive, enable_tcp_nodelay};

    #[cfg(unix)]
    pub use crate::ipc;
}
