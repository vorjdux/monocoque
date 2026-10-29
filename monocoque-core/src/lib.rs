//! Monocoque Core
//!
//! This crate contains the runtime-agnostic core building blocks:
//! - The socket multiplexer every socket is built on (`mux`)
//! - The message type (`msg`) and the single-thread queue (`localq`)
//! - The stream transport over TCP, IPC and inproc (`transport`)
//! - Shared owned-buffer I/O helpers for the runtime backends (`io`)
//! - TCP utilities for high-performance networking (`tcp`)
//! - The PUB/SUB subscription matcher (`subscription`)
//! - Error types (`error`)

// The tcp module needs raw fd/socket access for socket configuration
#![cfg_attr(not(test), deny(unsafe_code))]

pub mod endpoint;
pub mod error;
pub mod io;
pub mod localq;
pub mod monitor;
pub mod msg;
pub mod mux;
pub mod options;
pub mod reconnect;
pub mod rt;
pub mod subscription;
pub mod tcp;
pub mod timeout;
pub mod transport;

#[cfg(unix)]
pub mod ipc;

// Optional: a small prelude to make downstream crates ergonomic.
// Keep it minimal to avoid API lock-in.
pub mod prelude {
    pub use crate::endpoint::Endpoint;
    pub use crate::monitor::{SocketEvent, SocketMonitor};
    pub use crate::msg::{Msg, MsgRef};
    pub use crate::options::SocketOptions;
    pub use crate::reconnect::{ReconnectError, ReconnectState};
    pub use crate::subscription::{PeerKey, SubscriptionIndex};
    pub use crate::tcp::{configure_tcp_keepalive, enable_tcp_nodelay};

    #[cfg(unix)]
    pub use crate::ipc;
}
