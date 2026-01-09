//! Backpressure: `BytePermits`
//!
//! Byte-based flow control for write pumps.
//!
//! Design principle:
//! - Backpressure scales with **bytes**, not message count
//! - One giant message should not starve other connections
//! - Pluggable: `NoOp` (default) → Semaphore → dynamic policy
//!
//! Usage:
//! ```rust,ignore
//! let permit = permits.acquire(n_bytes).await;
//! writer.write(buf).await;
//! drop(permit); // releases automatically
//! ```

use async_trait::async_trait;

/// Backpressure permit trait.
///
/// Implementations control write pump flow based on byte counts.
#[async_trait]
pub trait BytePermits: Send + Sync {
    /// Acquire permission to write `n_bytes`.
    ///
    /// This may block if the system is under memory pressure.
    async fn acquire(&self, n_bytes: usize) -> Permit;
}

/// RAII permit guard.
///
/// Releases the permit when dropped.
pub struct Permit {
    // Future: store reference to semaphore for actual release
    _private: (),
}

impl Permit {
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

/// No-op implementation (Phase 0).
///
/// Always grants permits immediately.
/// Use this until memory pressure becomes an issue.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpPermits;

#[async_trait]
impl BytePermits for NoOpPermits {
    async fn acquire(&self, _n_bytes: usize) -> Permit {
        Permit::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_permits_always_succeed() {
        let permits = NoOpPermits;
        let rt = compio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let _p1 = permits.acquire(1024).await;
            let _p2 = permits.acquire(1_000_000).await;
            // Should not block
        });
    }
}
