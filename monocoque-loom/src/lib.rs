//! Loom concurrency-model tests for monocoque-zmtp's publisher atomics.
//!
//! These are *models* of the synchronization protocols in
//! `monocoque-zmtp/src/publisher.rs`, run in a standalone crate so `--cfg loom`
//! reaches only loom (the real crates pull flume/concurrent-queue, which have
//! their own cfg(loom) paths that break under a global flag). The production
//! union is guarded by a `parking_lot::RwLock`, which loom cannot model, so the
//! lock-protected payload is represented here by a plain atomic; what is being
//! verified is the generation counter's Acquire/Release pairing and the
//! Relaxed subscriber-count arithmetic, which are the parts loom can prove.
//!
//! Run: `RUSTFLAGS="--cfg loom" cargo test --manifest-path monocoque-loom/Cargo.toml`

#![cfg(loom)]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use loom::thread;

/// Models `SharedSubscriptions::update` vs the publisher prefilter: the writer
/// updates the union then bumps `generation` with Release; the publisher fast
/// path loads `generation` with Acquire and only re-reads the union when it
/// advanced. Observing a bumped generation MUST imply observing the union write
/// that preceded it. `data` stands in for the (elided, lock-protected) union.
#[test]
fn generation_release_acquire_publishes_union_update() {
    loom::model(|| {
        let data = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicU64::new(0));

        let w_data = Arc::clone(&data);
        let w_gen = Arc::clone(&generation);
        let writer = thread::spawn(move || {
            w_data.store(42, Ordering::Relaxed); // union write (under the elided lock)
            w_gen.fetch_add(1, Ordering::Release); // publish the new generation
        });

        // Publisher fast path: read generation, then the union.
        if generation.load(Ordering::Acquire) != 0 {
            assert_eq!(
                data.load(Ordering::Relaxed),
                42,
                "Acquire load of a bumped generation must publish the preceding union write"
            );
        }

        writer.join().unwrap();
    });
}

/// Models the per-worker subscriber count: subscribe does `fetch_add` and
/// dead-subscriber cleanup does `fetch_sub` (both Relaxed). Concurrent updates
/// must not lose or tear on any interleaving: the final count is the net.
#[test]
fn worker_sub_count_never_loses_updates() {
    loom::model(|| {
        let count = Arc::new(AtomicUsize::new(1)); // one existing subscriber

        let c_add = Arc::clone(&count);
        let adder = thread::spawn(move || {
            c_add.fetch_add(1, Ordering::Relaxed); // a new subscriber joins
        });

        // Concurrent cleanup removes the pre-existing subscriber.
        count.fetch_sub(1, Ordering::Relaxed);

        adder.join().unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1); // 1 + 1 - 1 on every interleaving
    });
}
