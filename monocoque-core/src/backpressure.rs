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

use async_trait::async_trait;
use parking_lot::{Condvar, Mutex};
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

/// Internal state for the byte semaphore.
struct SemInner {
    available: usize,
}

/// RAII permit guard.
///
/// Releases the permit when dropped.
pub struct Permit {
    inner: Option<PermitInner>,
}

enum PermitInner {
    /// Byte-counting semaphore backed by parking_lot primitives (usable in drop).
    ByteSem(Arc<(Mutex<SemInner>, Condvar)>, usize),
    NoOp,
}

impl Drop for Permit {
    fn drop(&mut self) {
        match self.inner.take() {
            Some(PermitInner::ByteSem(inner, n_bytes)) => {
                let (mutex, condvar) = &*inner;
                let mut guard = mutex.lock();
                guard.available += n_bytes;
                condvar.notify_all();
            }
            Some(PermitInner::NoOp) | None => {}
        }
    }
}

impl Permit {
    pub(crate) const fn noop() -> Self {
        Self {
            inner: Some(PermitInner::NoOp),
        }
    }

    fn byte_sem(inner: Arc<(Mutex<SemInner>, Condvar)>, n_bytes: usize) -> Self {
        Self {
            inner: Some(PermitInner::ByteSem(inner, n_bytes)),
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
/// Acquires all N bytes in a single atomic operation (O(1), not O(N)).
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
    inner: Arc<(Mutex<SemInner>, Condvar)>,
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
            inner: Arc::new((
                Mutex::new(SemInner { available: max_bytes }),
                Condvar::new(),
            )),
        }
    }
}

#[async_trait]
impl BytePermits for SemaphorePermits {
    async fn acquire(&self, n_bytes: usize) -> Permit {
        if n_bytes == 0 {
            return Permit::noop();
        }

        // Blocking wait is performed on a dedicated thread so we don't block
        // the async executor. parking_lot::Condvar::wait is synchronous and
        // safe to use here because SemInner uses parking_lot::Mutex.
        let inner = self.inner.clone();
        compio::runtime::spawn_blocking(move || {
            let (mutex, condvar) = &*inner;
            let mut guard = mutex.lock();
            // Wait until enough capacity is available.
            while guard.available < n_bytes {
                condvar.wait(&mut guard);
            }
            guard.available -= n_bytes;
            // guard is dropped here, releasing the lock before we return.
        })
        .await;

        Permit::byte_sem(self.inner.clone(), n_bytes)
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

    #[test]
    fn semaphore_permits_single_atomic_acquire() {
        // Verify that acquiring N bytes is done atomically (not O(N) individual acquires)
        let permits = SemaphorePermits::new(1024 * 1024); // 1MB
        let rt = compio::runtime::Runtime::new().unwrap();

        rt.block_on(async {
            // Acquire a large block in one shot - this should not loop N times
            let permit = permits.acquire(512 * 1024).await; // 512KB
            drop(permit);
        });
    }
}
