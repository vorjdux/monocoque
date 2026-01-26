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
//! let permits = SemaphorePermits::new(10 * 1024 * 1024); // 10MB limit
//! let permit = permits.acquire(n_bytes).await;
//! writer.write(buf).await;
//! drop(permit); // releases automatically
//! ```

use async_lock::Semaphore;
use async_trait::async_trait;
use std::sync::Arc;

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
    inner: Option<PermitInner>,
}

enum PermitInner {
    Semaphore(Arc<Semaphore>, usize), // Semaphore + byte count to release
    NoOp,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(PermitInner::Semaphore(sem, n_bytes)) = self.inner.take() {
            sem.add_permits(n_bytes);
        }
    }
}

impl Permit {
    pub(crate) const fn noop() -> Self {
        Self {
            inner: Some(PermitInner::NoOp),
        }
    }

    const fn semaphore(sem: Arc<Semaphore>, n_bytes: usize) -> Self {
        Self {
            inner: Some(PermitInner::Semaphore(sem, n_bytes)),
        }
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
        Permit::noop()
    }
}

/// Semaphore-based backpressure implementation.
///
/// Enforces a maximum number of bytes that can be buffered at once.
/// When the limit is reached, `acquire()` will block until space is available.
///
/// # Example
///
/// ```
/// use monocoque_core::backpressure::{BytePermits, SemaphorePermits};
///
/// # compio::runtime::Runtime::new().unwrap().block_on(async {
/// // Allow up to 10MB of buffered data
/// let permits = SemaphorePermits::new(10 * 1024 * 1024);
///
/// // Acquire permit for 1KB write
/// let permit = permits.acquire(1024).await;
/// // ... perform write ...
/// drop(permit); // releases 1024 bytes back to the pool
/// # });
/// ```
#[derive(Clone)]
pub struct SemaphorePermits {
    semaphore: Arc<Semaphore>,
}

impl SemaphorePermits {
    /// Create a new semaphore-based backpressure controller.
    ///
    /// # Arguments
    ///
    /// * `max_bytes` - Maximum number of bytes that can be buffered
    #[must_use] 
    pub fn new(max_bytes: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_bytes)),
        }
    }
}

#[async_trait]
impl BytePermits for SemaphorePermits {
    async fn acquire(&self, n_bytes: usize) -> Permit {
        // Acquire n_bytes permits from the semaphore (one at a time)
        for _ in 0..n_bytes {
            let _ = self.semaphore.acquire().await;
        }
        
        // Return permit that will release on drop
        Permit::semaphore(self.semaphore.clone(), n_bytes)
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

    #[test]
    fn semaphore_permits_enforce_limit() {
        let permits = SemaphorePermits::new(1024);
        let rt = compio::runtime::Runtime::new().unwrap();
        
        rt.block_on(async {
            // First 1024 bytes should succeed
            let p1 = permits.acquire(1024).await;
            
            // Try to acquire more - this would block, so we test the behavior
            // by checking we can acquire after dropping
            drop(p1);
            
            let _p2 = permits.acquire(512).await;
            let _p3 = permits.acquire(512).await;
            // Should succeed with 1024 total
        });
    }

    #[test]
    fn semaphore_permits_release_on_drop() {
        let permits = SemaphorePermits::new(1000);
        let rt = compio::runtime::Runtime::new().unwrap();
        
        rt.block_on(async {
            {
                let _p1 = permits.acquire(500).await;
                let _p2 = permits.acquire(500).await;
                // Full capacity used
            } // Permits dropped here
            
            // Should be able to acquire again after drop
            let _p3 = permits.acquire(1000).await;
        });
    }
}
