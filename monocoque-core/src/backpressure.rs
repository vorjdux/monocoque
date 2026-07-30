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

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};

/// Backpressure permit trait.
///
/// Implementations control write pump flow based on byte counts.
///
/// Native async-fn-in-trait rather than `#[async_trait]`: `acquire` is on the
/// per-write flow-control path, so the box-per-call async_trait added was pure
/// overhead. The trait is used behind generics, never as `dyn BytePermits`.
#[allow(async_fn_in_trait)]
pub trait BytePermits: Send + Sync {
    /// Acquire permission to write `n_bytes`.
    ///
    /// This may suspend (on the executor, never on a blocking thread) if the
    /// system is under memory pressure, resuming when enough capacity frees up.
    async fn acquire(&self, n_bytes: usize) -> Permit;
}

/// A waiter parked on the byte semaphore.
///
/// `granted`/`waker` are mutated only by whoever holds the `SemInner` lock (the
/// releaser that hands out capacity, or the waiter refreshing its waker), so the
/// atomic plus a small mutex are all the synchronization the slot needs.
struct WaiterSlot {
    /// Bytes this waiter is trying to claim (already clamped to `max_bytes`).
    needed: usize,
    /// Set true by the releaser once `needed` bytes have been deducted for this
    /// waiter. The waiter then converts that reservation into a `Permit`.
    granted: AtomicBool,
    /// Waker to notify when granted; refreshed by the waiter on each poll.
    waker: Mutex<Option<Waker>>,
}

/// Internal state for the byte semaphore, guarded by one mutex.
struct SemInner {
    /// Bytes currently free to hand out.
    available: usize,
    /// FIFO queue of parked waiters. Front-to-back granting gives fair ordering
    /// and bounds head-of-line waiting.
    waiters: VecDeque<Arc<WaiterSlot>>,
}

/// Hand freed capacity to the front waiters that now fit, in FIFO order.
///
/// Stops at the first waiter that still does not fit, so a single release wakes
/// only the waiters it can actually satisfy - never a thundering herd. Returns
/// the wakers to fire; the caller wakes them after dropping the `SemInner` lock.
fn grant_front(inner: &mut SemInner) -> Vec<Waker> {
    let mut wakers = Vec::new();
    loop {
        let Some(front) = inner.waiters.front() else {
            break;
        };
        if front.needed > inner.available {
            break;
        }
        let slot = inner.waiters.pop_front().expect("front just checked");
        inner.available -= slot.needed;
        slot.granted.store(true, Ordering::Release);
        let waker = slot.waker.lock().take();
        if let Some(w) = waker {
            wakers.push(w);
        }
    }
    wakers
}

/// RAII permit guard.
///
/// Releases the permit when dropped.
pub struct Permit {
    inner: Option<PermitInner>,
}

enum PermitInner {
    /// Byte-counting semaphore claim: returns `n_bytes` to the pool on drop.
    ByteSem(Arc<Mutex<SemInner>>, usize),
    NoOp,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(PermitInner::ByteSem(sem, n_bytes)) = self.inner.take() {
            // Return the bytes and immediately hand them to any waiters that now
            // fit. Wake outside the lock to avoid re-entering it from a waker.
            let wakers = {
                let mut inner = sem.lock();
                inner.available += n_bytes;
                let wakers = grant_front(&mut inner);
                drop(inner);
                wakers
            };
            for w in wakers {
                w.wake();
            }
        }
    }
}

impl Permit {
    pub(crate) const fn noop() -> Self {
        Self {
            inner: Some(PermitInner::NoOp),
        }
    }

    fn byte_sem(sem: Arc<Mutex<SemInner>>, n_bytes: usize) -> Self {
        Self {
            inner: Some(PermitInner::ByteSem(sem, n_bytes)),
        }
    }
}

/// No-op implementation (Phase 0).
///
/// Always grants permits immediately.
/// Use this until memory pressure becomes an issue.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpPermits;

impl BytePermits for NoOpPermits {
    async fn acquire(&self, _n_bytes: usize) -> Permit {
        Permit::noop()
    }
}

/// Semaphore-based backpressure implementation.
///
/// Enforces a maximum number of bytes that can be buffered at once. When the
/// limit is reached, `acquire()` suspends on the executor until enough capacity
/// is released, then resumes in FIFO order. Acquires all N bytes in a single
/// atomic operation (O(1), not O(N)).
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
    inner: Arc<Mutex<SemInner>>,
    /// Total capacity; oversized acquires clamp to this so they never wait for
    /// capacity that can never exist.
    max_bytes: usize,
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
            inner: Arc::new(Mutex::new(SemInner {
                available: max_bytes,
                waiters: VecDeque::new(),
            })),
            max_bytes,
        }
    }
}

impl BytePermits for SemaphorePermits {
    async fn acquire(&self, n_bytes: usize) -> Permit {
        if n_bytes == 0 {
            return Permit::noop();
        }
        // Clamp so a single oversized message consumes the whole pool instead of
        // waiting forever for capacity that cannot exist.
        let needed = n_bytes.min(self.max_bytes);
        Acquire {
            sem: self.inner.clone(),
            needed,
            slot: None,
            #[cfg(test)]
            counted_slow: false,
        }
        .await
    }
}

/// Future returned by `SemaphorePermits::acquire`.
///
/// On first poll it claims capacity outright if the pool is free and nobody is
/// queued ahead of it (preserving FIFO); otherwise it parks a `WaiterSlot` and
/// completes once the releaser grants it.
struct Acquire {
    sem: Arc<Mutex<SemInner>>,
    needed: usize,
    slot: Option<Arc<WaiterSlot>>,
    #[cfg(test)]
    counted_slow: bool,
}

impl Future for Acquire {
    type Output = Permit;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Permit> {
        // Acquire holds no self-references, so it is Unpin and get_mut is sound.
        let this = self.get_mut();

        // Already parked: complete once the releaser grants us.
        if let Some(slot) = &this.slot {
            if slot.granted.load(Ordering::Acquire) {
                let sem = this.sem.clone();
                this.slot = None; // taken; Drop must not refund it
                return Poll::Ready(Permit::byte_sem(sem, this.needed));
            }
            // Refresh the waker, then re-check granted to close the race where a
            // grant lands between the check above and storing the new waker.
            *slot.waker.lock() = Some(cx.waker().clone());
            if slot.granted.load(Ordering::Acquire) {
                let sem = this.sem.clone();
                this.slot = None;
                return Poll::Ready(Permit::byte_sem(sem, this.needed));
            }
            return Poll::Pending;
        }

        // First poll: claim immediately only if the pool is free and no one is
        // waiting ahead of us; otherwise queue to preserve FIFO fairness.
        let mut inner = this.sem.lock();
        if inner.waiters.is_empty() && inner.available >= this.needed {
            inner.available -= this.needed;
            drop(inner);
            return Poll::Ready(Permit::byte_sem(this.sem.clone(), this.needed));
        }

        #[cfg(test)]
        if !this.counted_slow {
            SLOW_PATH_ENTRIES.fetch_add(1, Ordering::Relaxed);
            this.counted_slow = true;
        }

        let slot = Arc::new(WaiterSlot {
            needed: this.needed,
            granted: AtomicBool::new(false),
            waker: Mutex::new(Some(cx.waker().clone())),
        });
        inner.waiters.push_back(slot.clone());
        drop(inner);
        this.slot = Some(slot);
        Poll::Pending
    }
}

impl Drop for Acquire {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        let wakers = {
            let mut inner = self.sem.lock();
            if slot.granted.load(Ordering::Acquire) {
                // Granted but never turned into a Permit (cancelled between wake
                // and poll): return the reserved bytes so they can be re-granted.
                inner.available += self.needed;
            } else {
                // Still queued: drop our slot. Removing a large front waiter can
                // expose smaller ones behind it, which the grant walk then wakes.
                inner.waiters.retain(|s| !Arc::ptr_eq(s, &slot));
            }
            let wakers = grant_front(&mut inner);
            drop(inner);
            wakers
        };
        for w in wakers {
            w.wake();
        }
    }
}

/// Counts how many `acquire` calls had to park a waiter (the slow path).
/// Test-only, used to prove the uncontended fast path claims capacity outright.
#[cfg(test)]
static SLOW_PATH_ENTRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn uncontended_acquire_takes_fast_path() {
        // A series of uncontended acquire/release cycles must never park a
        // waiter, so no queueing overhead is paid on the hot path.
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
            "uncontended acquires must not park a waiter"
        );
    }

    #[test]
    fn contended_acquire_parks_then_completes_on_release() {
        // With the pool exhausted, a second acquire must park and only complete
        // once the first permit is released - no blocking thread involved.
        let permits = SemaphorePermits::new(1024);
        let rt = crate::rt::LocalRuntime::new().unwrap();

        SLOW_PATH_ENTRIES.store(0, Ordering::Relaxed);
        rt.block_on(async {
            let p1 = permits.acquire(1024).await; // exhausts the pool

            let permits2 = permits.clone();
            let waiter = crate::rt::spawn(async move {
                // Cannot be satisfied until p1 is released.
                let _p2 = permits2.acquire(1024).await;
            });

            // Give the waiter a chance to park, then release.
            crate::rt::sleep(std::time::Duration::from_millis(50)).await;
            assert!(
                SLOW_PATH_ENTRIES.load(Ordering::Relaxed) >= 1,
                "the second acquire should have parked while the pool was full"
            );
            drop(p1);
            crate::rt::join(waiter).await;
        });
    }

    #[test]
    fn waiters_are_granted_in_fifo_order() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let permits = SemaphorePermits::new(1024);
        let rt = crate::rt::LocalRuntime::new().unwrap();
        let order = Rc::new(RefCell::new(Vec::new()));

        rt.block_on(async {
            let p = permits.acquire(1024).await; // exhaust

            // Queue three waiters in a known order, each needing the full pool.
            let mut handles = Vec::new();
            for id in 0..3 {
                let permits_i = permits.clone();
                let order_i = order.clone();
                handles.push(crate::rt::spawn(async move {
                    let _permit = permits_i.acquire(1024).await;
                    order_i.borrow_mut().push(id);
                    // Hold briefly so the next waiter is granted only after this
                    // one releases, making the observed order deterministic.
                    crate::rt::sleep(std::time::Duration::from_millis(10)).await;
                }));
            }

            crate::rt::sleep(std::time::Duration::from_millis(50)).await;
            drop(p); // wakes waiter 0; each release chains to the next
            for h in handles {
                crate::rt::join(h).await;
            }
        });

        assert_eq!(
            *order.borrow(),
            vec![0, 1, 2],
            "waiters must be granted in the order they queued"
        );
    }

    #[test]
    fn cancelled_waiter_does_not_leak_capacity() {
        // A waiter that is polled (parked) and then dropped before being granted
        // must remove itself so its slot does not wedge the queue, and must not
        // consume capacity.
        let permits = SemaphorePermits::new(1024);
        let rt = crate::rt::LocalRuntime::new().unwrap();

        rt.block_on(async {
            let p1 = permits.acquire(1024).await; // exhaust

            // Park a waiter, then cancel it.
            {
                let mut fut = Box::pin(permits.acquire(512));
                let polled = futures::poll!(fut.as_mut());
                assert!(polled.is_pending(), "waiter should park while pool is full");
                drop(fut); // cancel the parked waiter
            }

            // Releasing p1 restores the full pool; a fresh acquire of the whole
            // pool must succeed, proving the cancelled waiter left nothing behind.
            drop(p1);
            let _p2 = permits.acquire(1024).await;
        });
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
