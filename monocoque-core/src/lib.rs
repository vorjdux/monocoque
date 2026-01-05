//! Monocoque Core
//!
//! This crate contains the runtime-agnostic core building blocks:
//! - Pinned / io_uring-safe allocation (`alloc`)
//! - Split-pump socket actor (`actor`)
//! - ROUTER hub + peer map (`router`)
//! - PUB/SUB core (subscription index + hub) (`pubsub`)
//! - Byte-based backpressure (`backpressure`)
//! - Error types (`error`)

#![deny(unsafe_code)]
pub mod actor;
pub mod alloc;
pub mod backpressure;
pub mod error;
pub mod router;

pub mod pubsub {
    pub mod hub;
    pub mod index;
}

// Optional: a small prelude to make downstream crates ergonomic.
// Keep it minimal to avoid API lock-in.
pub mod prelude {
    pub use crate::actor::{SocketActor, SocketEvent, UserCmd};
    pub use crate::alloc::{IoArena, SlabMut};
    pub use crate::backpressure::{BytePermits, NoOpPermits, Permit};
    pub use crate::pubsub::hub::{PubSubCmd, PubSubEvent, PubSubHub};
    pub use crate::pubsub::index::{PeerKey, SubscriptionIndex};
    pub use crate::router::{HubEvent, PeerCmd, RouterBehavior, RouterCmd, RouterHub};
}
