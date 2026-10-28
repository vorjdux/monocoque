//! ZMTP decoder configuration constants.

/// Initial staging buffer capacity for decoder reassembly (256 bytes)
///
/// Pre-allocated to avoid initial reallocation on fragmented frames.
/// Only used when frame spans multiple segments (slow path).
pub const STAGING_BUF_INITIAL_CAP: usize = 256;
