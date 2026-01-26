//! RAII guard for protecting against partial I/O corruption in async contexts.
//!
//! # The Problem
//!
//! In async Rust, when a Future is dropped (e.g., due to timeout), execution stops
//! immediately. If this happens during a multi-step I/O operation like writing a
//! multipart ZMTP message, the underlying stream is left in an undefined state
//! with potentially half a frame written.
//!
//! # The Solution
//!
//! The `PoisonGuard` uses RAII to track whether a critical I/O section completed
//! successfully:
//!
//! 1. `PoisonGuard::new()` sets a flag to `true` (assume failure)
//! 2. If the Future is dropped before completion, the flag remains `true`
//! 3. Only by calling `disarm()` after successful I/O does the flag reset to `false`
//!
//! # Example
//!
//! ```rust
//! use monocoque_core::poison::PoisonGuard;
//!
//! struct MySocket {
//!     is_poisoned: bool,
//!     // ... other fields
//! }
//!
//! impl MySocket {
//!     async fn send(&mut self, data: &[u8]) -> std::io::Result<()> {
//!         // Check health before attempting I/O
//!         if self.is_poisoned {
//!             return Err(std::io::Error::new(
//!                 std::io::ErrorKind::BrokenPipe,
//!                 "Socket poisoned by cancelled I/O"
//!             ));
//!         }
//!
//!         // Arm the guard - if dropped, socket remains poisoned
//!         let guard = PoisonGuard::new(&mut self.is_poisoned);
//!
//!         // Critical section: if this is cancelled, guard drops
//!         // and socket remains poisoned
//!         // ... perform I/O operations ...
//!
//!         // Success! Disarm the guard
//!         guard.disarm();
//!         Ok(())
//!     }
//! }
//! ```
//!
//! # When to Use
//!
//! Apply this to **every** function that performs non-atomic writes:
//! - Writing multipart messages
//! - Flushing buffered data
//! - Any write operation larger than MTU
//! - Sequential writes that form a logical unit
//!
//! # When NOT to Use
//!
//! Typically don't use for reads (they're usually idempotent), unless:
//! - Reading multipart data where internal state changes
//! - State transitions that can't be rolled back
//!
//! # Critical Rules
//!
//! 1. **Only disarm when the entire logical operation completes**
//! 2. **Never manually reset `is_poisoned` after an error**
//! 3. **Once poisoned, the connection must be dropped and reconnected**

/// A RAII guard that marks a connection as poisoned if dropped before disarmed.
///
/// This is a structural guarantee that protects protocol integrity when async
/// operations are cancelled (e.g., by timeouts).
///
/// # Safety
///
/// The guard must live across the entire critical section. Dropping it early
/// or disarming before all I/O completes defeats its purpose.
pub struct PoisonGuard<'a> {
    flag: &'a mut bool,
}

impl<'a> PoisonGuard<'a> {
    /// Create a new guard, immediately marking the connection as poisoned.
    ///
    /// The connection will remain poisoned unless `disarm()` is called.
    #[inline]
    pub fn new(flag: &'a mut bool) -> Self {
        *flag = true;
        Self { flag }
    }

    /// Disarm the guard, marking the connection as healthy.
    ///
    /// **Only call this when the entire I/O operation has completed successfully.**
    ///
    /// # Example
    ///
    /// ```rust
    /// # use monocoque_core::poison::PoisonGuard;
    /// # async fn example() -> std::io::Result<()> {
    /// # let mut is_poisoned = false;
    /// let guard = PoisonGuard::new(&mut is_poisoned);
    ///
    /// // Perform all I/O operations
    /// // ...
    ///
    /// // Only disarm after everything succeeds
    /// guard.disarm();
    /// # Ok(())
    /// # }
    /// ```
    #[inline]
    pub fn disarm(self) {
        *self.flag = false;
        // self is dropped here, but since we updated the reference,
        // the connection is now marked as healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poison_on_drop() {
        let mut poisoned = false;
        {
            let _guard = PoisonGuard::new(&mut poisoned);
            // Guard dropped without disarm
        }
        assert!(poisoned, "Connection should be poisoned when guard is dropped");
    }

    #[test]
    fn test_disarm_clears_poison() {
        let mut poisoned = false;
        {
            let guard = PoisonGuard::new(&mut poisoned);
            // Can't check poisoned here - it's mutably borrowed by guard
            guard.disarm();
        }
        assert!(!poisoned, "Connection should be healthy after disarm");
    }

    #[test]
    fn test_disarm_at_end() {
        let mut poisoned = false;
        {
            let guard = PoisonGuard::new(&mut poisoned);
            // Simulate successful I/O
            guard.disarm();
            // Can only check after guard is dropped/disarmed
        }
        assert!(!poisoned);
    }

    #[test]
    fn test_early_drop() {
        let mut poisoned = false;
        {
            let guard = PoisonGuard::new(&mut poisoned);
            // Simulate cancelled operation - drop without disarm
            drop(guard);
            // Can only check after guard is dropped
        }
        assert!(poisoned, "Should remain poisoned on early drop");
    }
}
