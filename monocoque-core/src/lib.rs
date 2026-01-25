//! Monocoque Core
//!
//! This crate contains the runtime-agnostic core building blocks:
//! - Pinned / io_uring-safe allocation (`alloc`)
//! - Zero-copy segmented buffer (`buffer`)
//! - TCP utilities for high-performance networking (`tcp`)
//! - ROUTER hub + peer map (`router`)
//! - PUB/SUB core (subscription index + hub) (`pubsub`)
//! - Byte-based backpressure (`backpressure`)
//! - Error types (`error`)

// The tcp module needs raw fd/socket access for socket configuration
#![cfg_attr(not(test), deny(unsafe_code))]
// Allow some pedantic lints that are intentional in this crate
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::needless_pass_by_ref_mut)]
#![allow(clippy::match_same_arms)]
pub mod alloc;
pub mod backpressure;
pub mod buffer;
pub mod config;
pub mod endpoint;
pub mod error;
pub mod inproc;
pub mod message;
pub mod message_builder;
pub mod monitor;
pub mod options;
pub mod poison;
pub mod reconnect;
pub mod router;
pub mod socket_type;
pub mod subscription;
pub mod tcp;
pub mod timeout;

#[cfg(unix)]
pub mod ipc;

pub mod pubsub {
    pub mod hub;
    pub mod index;
}

// Optional: a small prelude to make downstream crates ergonomic.
// Keep it minimal to avoid API lock-in.
pub mod prelude {
    pub use crate::alloc::{IoArena, SlabMut};
    pub use crate::backpressure::{BytePermits, NoOpPermits, Permit, SemaphorePermits};
    pub use crate::buffer::SegmentedBuffer;
    pub use crate::endpoint::Endpoint;
    pub use crate::message_builder::Message;
    pub use crate::monitor::{SocketEvent, SocketMonitor};
    pub use crate::options::SocketOptions;
    pub use crate::poison::PoisonGuard;
    pub use crate::reconnect::{ReconnectError, ReconnectState};
    pub use crate::socket_type::SocketType;
    pub use crate::pubsub::hub::{PubSubCmd, PubSubEvent, PubSubHub};
    pub use crate::pubsub::index::{PeerKey, SubscriptionIndex};
    pub use crate::router::{HubEvent, PeerCmd, RouterBehavior, RouterCmd, RouterHub};
    pub use crate::tcp::{configure_tcp_keepalive, enable_tcp_nodelay};

    #[cfg(unix)]
    pub use crate::ipc;
}
