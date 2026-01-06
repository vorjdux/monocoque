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
// Allow some pedantic lints that are intentional in this crate
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::needless_pass_by_ref_mut)]
#![allow(clippy::match_same_arms)]
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
