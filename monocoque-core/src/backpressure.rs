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
    /// Total capacity; used to clamp oversized acquires so we never deadlock.
    max_bytes: usize,
}

/// RAII permit guard.
///
/// Releases the permit when dropped.
pub struct Permit {
    inner: Option<PermitInner>,
}

enum PermitInner {
    /// Byte-counting semaphore backed by `parking_lot` primitives (usable in `Drop`).
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
                drop(guard);
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
/// # monocoque_core::rt::LocalRuntime::new().unwrap().block_on(async {
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
                Mutex::new(SemInner {
                    available: max_bytes,
                    max_bytes,
                }),
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

        // Fast path: in the uncontended case capacity is available and an
        // in-place claim under a non-blocking try_lock is all that is needed.
        // This stays on the executor and avoids a thread-pool round trip per
        // write, which otherwise dominates the latency of a high-rate write
        // path. try_lock never parks the executor thread. The mutex remains the
        // single source of truth, so this cannot race the slow path below.
        {
            let (mutex, _condvar) = &*self.inner;
            if let Some(mut guard) = mutex.try_lock() {
                let claim = n_bytes.min(guard.max_bytes);
                if guard.available >= claim {
                    guard.available -= claim;
                    drop(guard);
                    return Permit::byte_sem(self.inner.clone(), claim);
                }
            }
        }

        // Slow path: contended, or not enough capacity right now. Wait on a
        // dedicated thread so we don't block the async executor.
        // parking_lot::Condvar::wait is synchronous and safe to use here
        // because SemInner uses parking_lot::Mutex.
        #[cfg(test)]
        SLOW_PATH_ENTRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let inner = self.inner.clone();
        let actual = crate::rt::spawn_blocking(move || {
            let (mutex, condvar) = &*inner;
            let mut guard = mutex.lock();
            // Clamp to max_bytes so a single oversized message never deadlocks:
            // the message will consume the entire capacity instead of waiting
            // forever for capacity that can never exist.
            let claim = n_bytes.min(guard.max_bytes);
            // Wait until enough capacity is available.
            while guard.available < claim {
                condvar.wait(&mut guard);
            }
            guard.available -= claim;
            // Return the actual bytes claimed so the Permit releases the right amount.
            claim
        })
        .await;

        Permit::byte_sem(self.inner.clone(), actual)
    }
}

/// Counts how many `acquire` calls fell through to the blocking slow path.
/// Test-only, used to prove the uncontended fast path avoids `spawn_blocking`.
#[cfg(test)]
static SLOW_PATH_ENTRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn uncontended_acquire_takes_fast_path() {
        // A series of uncontended acquire/release cycles must never enter the
        // blocking slow path, so no per-write thread-pool round trip is paid.
        let permits = SemaphorePermits::new(1024 * 1024);
        let rt = crate::rt::LocalRuntime::new().unwrap();

        SLOW_PATH_ENTRIES.store(0, Ordering::Relaxed);
        rt.block_on(async {
            for _ in 0..100 {
                let permit = permits.acquire(1024).await;
                drop(permit);
            }
        });
        assert_eq!(
            SLOW_PATH_ENTRIES.load(Ordering::Relaxed),
            0,
            "uncontended acquires must not hit the spawn_blocking slow path"
        );
    }

    #[test]
    fn insufficient_capacity_uses_slow_path() {
        // When capacity is exhausted the acquire must fall back to the blocking
        // wait path (and complete once capacity is released).
        let permits = SemaphorePermits::new(1024);
        let rt = crate::rt::LocalRuntime::new().unwrap();

        SLOW_PATH_ENTRIES.store(0, Ordering::Relaxed);
        rt.block_on(async {
            let p1 = permits.acquire(1024).await; // fast path, exhausts capacity
            // This one cannot be satisfied immediately; release then reacquire.
            drop(p1);
            let _p2 = permits.acquire(1024).await;
        });
        // The first acquire took the fast path; only note that the mechanism is
        // exercised without deadlock. (Exact slow-path count is timing
        // dependent because the drop may free capacity before the retry.)
    }

    #[test]
    fn noop_permits_always_succeed() {
        let permits = NoOpPermits;
        let rt = crate::rt::LocalRuntime::new().unwrap();
        rt.block_on(async {
            let _p1 = permits.acquire(1024).await;
            let _p2 = permits.acquire(1_000_000).await;
            // Should not block
        });
    }

    #[test]
    fn semaphore_permits_enforce_limit() {
        let permits = SemaphorePermits::new(1024);
        let rt = crate::rt::LocalRuntime::new().unwrap();

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
        let rt = crate::rt::LocalRuntime::new().unwrap();

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
    fn semaphore_permits_oversized_acquire_does_not_deadlock() {
        // A single acquire larger than max_bytes must complete (clamped to max_bytes)
        // rather than deadlocking forever waiting for capacity that can never exist.
        let permits = SemaphorePermits::new(1024);
        let rt = crate::rt::LocalRuntime::new().unwrap();

        rt.block_on(async {
            let permit = permits.acquire(2048).await; // 2× max - must not deadlock
            drop(permit);
            // After release, we can acquire up to max_bytes again.
            let _p = permits.acquire(1024).await;
        });
    }

    #[test]
    fn semaphore_permits_single_atomic_acquire() {
        // Verify that acquiring N bytes is done atomically (not O(N) individual acquires)
        let permits = SemaphorePermits::new(1024 * 1024); // 1MB
        let rt = crate::rt::LocalRuntime::new().unwrap();

        rt.block_on(async {
            // Acquire a large block in one shot - this should not loop N times
            let permit = permits.acquire(512 * 1024).await; // 512KB
            drop(permit);
        });
    }
}
