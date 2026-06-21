//! Message builder for ergonomic multipart message construction.
//!
//! Re-exports [`crate::message_builder::Message`] as the canonical `Message` type.
//! All functionality lives in [`crate::message_builder`]; this module exists for
//! backward-compatibility so that `monocoque_core::message::Message` continues to work.

/// Re-export the canonical `Message` builder from `message_builder`.
pub use crate::message_builder::Message;
