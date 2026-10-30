//! Loom models of monocoque's cross-thread wakeup protocols.
//!
//! Each test restates one protocol with loom primitives, using the same
//! orderings as the production code, and loom explores every interleaving for
//! a lost wakeup. The models live in a standalone crate so `--cfg loom` reaches
//! only loom: the real crates pull flume, concurrent-queue and smol, which have
//! their own `cfg(loom)` paths that break when the flag is forced on globally.
//! Keep a model in step with the code it names when that code changes.
//!
//! Run: `RUSTFLAGS="--cfg loom" cargo test --release --manifest-path monocoque-loom/Cargo.toml`

#![cfg(all(loom, test))]

use loom::sync::atomic::{AtomicBool, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

/// `monocoque-core/src/mux/inproc.rs`, `RemoteTx::watch_room` against
/// `RemoteRx::recv`. A sender that found the cross-thread channel at its HWM
/// registers its waker, raises `waiting` and checks for room again; a receiver
/// takes a message, then clears `waiting` and wakes the sender if it was set.
/// Whatever the interleaving, the sender must either see the room or be woken.
///
/// The channel is a lock, as flume's is: `len()` and the receiver's take both
/// lock it, and that lock is what orders the sender's `waiting` store against
/// the receiver's swap. (Modeling it with bare atomics would be the
/// store-buffering pattern, which loom reports even with `SeqCst` because it
/// treats `SeqCst` as acquire/release.)
#[test]
fn inproc_room_signal_never_loses_the_sender() {
    const HWM: usize = 1;
    loom::model(|| {
        // The channel starts full; the receiver is about to take one message.
        let len = Arc::new(Mutex::new(HWM));
        let waiting = Arc::new(AtomicBool::new(false));
        let woken = Arc::new(AtomicBool::new(false));

        let (r_len, r_waiting, r_woken) = (len.clone(), waiting.clone(), woken.clone());
        let receiver = thread::spawn(move || {
            *r_len.lock().unwrap() -= 1;
            if r_waiting.swap(false, Ordering::SeqCst) {
                r_woken.store(true, Ordering::SeqCst);
            }
        });

        // watch_room: the waker is registered before `waiting` is raised.
        waiting.store(true, Ordering::SeqCst);
        let saw_room = *len.lock().unwrap() < HWM;

        receiver.join().unwrap();
        assert!(
            saw_room || woken.load(Ordering::SeqCst),
            "the sender neither saw room nor was woken"
        );
    });
}

/// The routing state of `monocoque-core/src/mux/children.rs` `Wakeups`, as far
/// as the cross-thread race needs it.
struct Wakeups {
    woken: Mutex<Vec<u32>>,
    pending: AtomicBool,
    active: AtomicBool,
    /// Set when someone is woken to poll the queued children.
    delivered: AtomicBool,
}

impl Wakeups {
    fn new() -> Self {
        Self {
            woken: Mutex::new(Vec::new()),
            pending: AtomicBool::new(false),
            active: AtomicBool::new(true),
            delivered: AtomicBool::new(false),
        }
    }

    /// `notify` from another thread (smol's reactor, a cross-thread inproc
    /// peer). `trust_active` is the shortcut production takes only on the
    /// owner thread.
    fn notify_foreign(&self, id: u32, trust_active: bool) {
        let mut s = self.woken.lock().unwrap();
        s.push(id);
        self.pending.store(true, Ordering::Release);
        if trust_active && self.active.load(Ordering::Acquire) {
            return;
        }
        drop(s);
        self.delivered.store(true, Ordering::SeqCst);
    }

    /// `exit` on the owner thread: the operation is returning, so anything
    /// queued after its last drain is delivered now.
    fn exit(&self) {
        self.active.store(false, Ordering::Release);
        if !self.pending.load(Ordering::Acquire) {
            return;
        }
        if !self.woken.lock().unwrap().is_empty() {
            self.delivered.store(true, Ordering::SeqCst);
        }
    }
}

fn run_notify_against_exit(trust_active: bool) {
    loom::model(move || {
        let w = Arc::new(Wakeups::new());
        let foreign = {
            let w = w.clone();
            thread::spawn(move || w.notify_foreign(7, trust_active))
        };
        w.exit();
        foreign.join().unwrap();
        assert!(
            w.delivered.load(Ordering::SeqCst),
            "a child was queued and nobody was woken to poll it"
        );
    });
}

/// The production protocol: a wakeup from another thread always delivers, so
/// it can never be stranded by an operation that is just returning.
#[test]
fn foreign_wakeup_is_never_lost_against_exit() {
    run_notify_against_exit(false);
}

/// Why it must: a foreign waker that trusted `active` can see it still set
/// while `exit` stores `false` and loads `pending` before the push is visible,
/// and then neither side delivers. Loom has to find that interleaving.
#[test]
#[should_panic(expected = "nobody was woken")]
fn trusting_active_from_another_thread_loses_wakeups() {
    run_notify_against_exit(true);
}
