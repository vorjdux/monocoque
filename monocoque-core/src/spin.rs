//! How long to keep looking for a message before going to sleep.
//!
//! Sleeping and being woken costs more than a round trip on a local socket,
//! and the sender pays for the wakeup inside its `send`.

use std::task::Context;
use std::time::{Duration, Instant};

/// Default time to look before sleeping. A REQ/REP round trip needs little:
/// on a Unix socket (64 B, two cores) 5 µs and 20 µs measured alike, 7.0 µs
/// against 10.0 without spinning. A peer answering a batch takes longer. A
/// DEALER sending 100 messages of 256 B to a ROUTER that echoes them waits 22
/// to 30 µs for the reply on an EPYC 7232P, so a 20 µs spin gave up just
/// before it came and then paid for the sleep as well: 6.63 ms per 100
/// batches at 20 µs, 6.02 ms at 30 µs and 5.80 ms at 40 µs and above. Latency
/// and one-way throughput measured the same at 20 µs and 50 µs.
#[cfg(not(any(feature = "runtime-smol", feature = "runtime-tokio")))]
const DEFAULT_SPIN: Duration = Duration::from_micros(50);
/// tokio has the whole gain at 30 µs (the same batches of 256 B on the
/// efficiency cores of an i7-1355U: 4.76 ms at 20 µs, 4.19 ms at 30 µs,
/// 4.15 ms at 50 µs), and its batches of 16 KiB measured better at 30 µs than
/// at 50 µs there (72 to 75 ms against 83 ms).
#[cfg(all(feature = "runtime-tokio", not(feature = "runtime-smol")))]
const DEFAULT_SPIN: Duration = Duration::from_micros(30);
/// smol wakes a sleeping thread through one reactor shared by the whole
/// process, re-armed on every wait, so a sleep costs far more there. One
/// sender feeding four workers gives each a batch every few dozen
/// microseconds: at 20 µs each worker slept between them and the pool ran at
/// half of 0.4's rate (5.4 M against 9.8 M msg/s at 64 B); at 100 µs it
/// passed it (10.3 M).
#[cfg(feature = "runtime-smol")]
const DEFAULT_SPIN: Duration = Duration::from_micros(100);

/// The spin budget.
///
/// What the socket or its runtime was asked for (zero disables), otherwise 50 µs
/// on compio, 30 µs on tokio and 100 µs on smol, or none when this thread may
/// run on one CPU only, where spinning only delays the peer it waits for.
///
/// A budget above [`MAX_SPIN`] is lowered to it.
#[must_use]
pub fn spin_budget(asked: Option<Duration>) -> Duration {
    asked.map_or_else(
        || {
            if single_cpu() {
                Duration::ZERO
            } else {
                DEFAULT_SPIN
            }
        },
        |asked| asked.min(MAX_SPIN),
    )
}

/// The longest spin.
///
/// Past this a thread is not looking before it sleeps, it is not sleeping,
/// and the end of a spin is worked out by adding the budget to the clock,
/// which a budget near `Duration::MAX` overflows.
pub const MAX_SPIN: Duration = Duration::from_secs(1);

/// `true` when this thread has one CPU to run on. On Linux the count honours
/// the affinity mask and the cgroup CPU quota.
///
/// Asked once per process, by whichever thread makes the first socket.
/// Finding the quota reads a file for each level of the cgroup tree, about
/// thirty system calls, which every new socket paid. A thread pinned to one
/// CPU inside a process that has several is not seen: a zero
/// `SocketOptions::with_recv_spin` or `LocalRuntime::with_spin` is for that.
fn single_cpu() -> bool {
    static SINGLE_CPU: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SINGLE_CPU.get_or_init(|| std::thread::available_parallelism().map_or(true, |n| n.get() <= 1))
}

/// A receive about to sleep yields instead, for up to the budget.
///
/// The runtime turns over and polls its reactor meanwhile. For runtimes whose
/// loop this crate does not drive; compio's `block_on` does the same itself.
pub struct YieldSpin {
    asked: Option<Duration>,
    /// Settled by the first wait: a socket may be made before the runtime
    /// that will run it is entered.
    budget: Option<Duration>,
    deadline: Option<Instant>,
    backoff: SpinBackoff,
}

impl YieldSpin {
    /// A spin of the budget the socket's options ask for. When they ask for
    /// none it is the one given to the `LocalRuntime` the socket runs on, and
    /// the default one when that has none either.
    #[must_use]
    pub fn new(asked: Option<Duration>) -> Self {
        Self {
            asked,
            budget: if cfg!(feature = "runtime-compio") {
                Some(Duration::ZERO)
            } else {
                None
            },
            deadline: None,
            backoff: SpinBackoff::default(),
        }
    }
}

thread_local! {
    /// The budget given to the `LocalRuntime` this thread is running, if any.
    static RUNTIME_SPIN: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// Makes a runtime's budget the one its thread's sockets use, for as long as
/// the runtime runs. Dropping it puts the earlier one back.
#[cfg(not(feature = "runtime-compio"))]
pub struct RuntimeSpin(Option<Duration>);

#[cfg(not(feature = "runtime-compio"))]
impl RuntimeSpin {
    /// Enter a runtime given `spin`. A runtime given none keeps the budget of
    /// the one it was entered from.
    #[must_use]
    pub fn enter(spin: Option<Duration>) -> Self {
        let outer = RUNTIME_SPIN.get();
        RUNTIME_SPIN.set(spin.or(outer));
        Self(outer)
    }
}

#[cfg(not(feature = "runtime-compio"))]
impl Drop for RuntimeSpin {
    fn drop(&mut self) {
        RUNTIME_SPIN.set(self.0);
    }
}

thread_local! {
    /// The socket that last came to wait on this thread, by the address of
    /// its [`YieldSpin`]. Zero until one has.
    static LAST_WAITER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl YieldSpin {
    /// `true` if the caller should return `Pending` with its waker already
    /// woken, instead of parking.
    ///
    /// A spin is for a thread with nothing else to do: it keeps the thread
    /// busy so that the reply, coming from another thread, is seen the
    /// moment it lands. When another socket on this thread comes to wait
    /// while this one spins, the thread does have something else to do, and
    /// very likely that other socket is the peer, which cannot be handed its
    /// bytes while the thread never gets round to its reactor. A DEALER and a
    /// ROUTER echoing to each other on one thread took 611 µs for a thousand
    /// messages on smol with the spin, against 207 µs without it. So the
    /// spin stops there and stays off for a good while.
    pub fn instead_of_park(&mut self, cx: &Context<'_>) -> bool {
        let budget = match self.budget {
            Some(budget) => budget,
            None => *self
                .budget
                .insert(spin_budget(self.asked.or_else(|| RUNTIME_SPIN.get()))),
        };
        // A socket that does not spin is recorded all the same: it is as
        // much a sign that the thread is shared as one that does.
        let me = std::ptr::from_ref(self).addr();
        let another_waited = LAST_WAITER.replace(me) != me;
        if budget.is_zero() {
            return false;
        }
        if another_waited && self.deadline.take().is_some() {
            self.backoff.thread_is_shared();
            return false;
        }
        let now = Instant::now();
        let deadline = match self.deadline {
            Some(d) => d,
            None if self.backoff.should_spin() => *self.deadline.insert(now + budget),
            None => return false,
        };
        if now < deadline {
            cx.waker().wake_by_ref();
            return true;
        }
        self.deadline = None;
        self.backoff.miss();
        false
    }

    /// A receive found a message. Costs one branch when not spinning.
    #[inline]
    pub const fn found(&mut self) {
        if self.deadline.is_some() {
            self.deadline = None;
            self.backoff.hit();
        }
    }
}

/// Stops spinning while it keeps finding nothing (an idle socket, or a peer
/// starved of CPU), and tries again after a while.
#[derive(Default)]
pub struct SpinBackoff {
    misses: u32,
    skip: u32,
}

impl SpinBackoff {
    /// Spins that find nothing, in a row, before spinning pauses.
    const MAX_MISSES: u32 = 8;
    /// Sleeps without spinning before trying again.
    const PAUSE: u32 = 64;

    /// Whether this wait may spin.
    pub const fn should_spin(&mut self) -> bool {
        if self.skip > 0 {
            self.skip -= 1;
            return false;
        }
        true
    }

    /// Sleeps without spinning after another socket was seen waiting on the
    /// same thread. Each new try costs that socket one delayed turn, so they
    /// are kept rare.
    const SHARED_THREAD_PAUSE: u32 = 1024;

    /// A spin found work.
    pub const fn hit(&mut self) {
        self.misses = 0;
    }

    /// Another socket came to wait on this thread during a spin.
    pub const fn thread_is_shared(&mut self) {
        self.misses = 0;
        self.skip = Self::SHARED_THREAD_PAUSE;
    }

    /// A spin ran out of budget.
    pub const fn miss(&mut self) {
        self.misses += 1;
        if self.misses >= Self::MAX_MISSES {
            self.misses = 0;
            self.skip = Self::PAUSE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Waker;

    fn spinner() -> YieldSpin {
        YieldSpin {
            asked: None,
            budget: Some(Duration::from_secs(60)),
            deadline: None,
            backoff: SpinBackoff::default(),
        }
    }

    #[test]
    fn a_budget_that_was_asked_for_replaces_the_default() {
        assert_eq!(spin_budget(Some(Duration::ZERO)), Duration::ZERO);
        assert_eq!(
            spin_budget(Some(Duration::from_micros(7))),
            Duration::from_micros(7)
        );
        let cx = Context::from_waker(Waker::noop());
        assert!(!YieldSpin::new(Some(Duration::ZERO)).instead_of_park(&cx));
    }

    #[test]
    fn a_budget_too_long_to_add_to_the_clock_is_lowered() {
        assert_eq!(spin_budget(Some(Duration::MAX)), MAX_SPIN);
        let cx = Context::from_waker(Waker::noop());
        let mut forever = YieldSpin {
            asked: Some(Duration::MAX),
            budget: None,
            deadline: None,
            backoff: SpinBackoff::default(),
        };
        assert!(forever.instead_of_park(&cx), "spins, and does not overflow");
    }

    #[test]
    fn a_socket_that_does_not_spin_still_ends_the_spin_of_another() {
        let cx = Context::from_waker(Waker::noop());
        let mut spinning = spinner();
        let mut quiet = YieldSpin {
            asked: None,
            budget: Some(Duration::ZERO),
            deadline: None,
            backoff: SpinBackoff::default(),
        };
        assert!(spinning.instead_of_park(&cx), "begins to spin");
        assert!(!quiet.instead_of_park(&cx), "never spins");
        assert!(
            !spinning.instead_of_park(&cx),
            "saw the other wait and sleeps"
        );
    }

    #[cfg(not(feature = "runtime-compio"))]
    #[test]
    fn a_socket_takes_the_budget_of_the_runtime_it_first_waits_on() {
        let cx = Context::from_waker(Waker::noop());
        let minute = Duration::from_secs(60);
        let mut made_outside = YieldSpin::new(None);
        let mut own_budget = YieldSpin::new(Some(minute));
        {
            let _runtime = RuntimeSpin::enter(Some(Duration::ZERO));
            assert!(!made_outside.instead_of_park(&cx), "the runtime's zero");
            let _inner = RuntimeSpin::enter(None);
            assert!(
                !YieldSpin::new(None).instead_of_park(&cx),
                "kept by a runtime given none"
            );
            assert!(
                own_budget.instead_of_park(&cx),
                "the socket's own comes first"
            );
        }
        assert_eq!(RUNTIME_SPIN.get(), None, "put back on the way out");
    }

    #[test]
    fn a_socket_alone_on_its_thread_keeps_spinning() {
        let cx = Context::from_waker(Waker::noop());
        let mut alone = spinner();
        for _ in 0..100 {
            assert!(alone.instead_of_park(&cx));
        }
        alone.found();
        assert!(alone.instead_of_park(&cx), "the next wait spins again");
    }

    #[test]
    fn another_socket_waiting_on_the_thread_ends_the_spin() {
        let cx = Context::from_waker(Waker::noop());
        let mut a = spinner();
        let mut b = spinner();
        assert!(a.instead_of_park(&cx), "a begins to spin");
        assert!(b.instead_of_park(&cx), "b begins to spin");
        assert!(!a.instead_of_park(&cx), "a saw b wait and sleeps");
        assert!(!b.instead_of_park(&cx), "b saw a wait and sleeps");

        // Both stay off for a good while, though each sees the other wait.
        for _ in 0..100 {
            assert!(!a.instead_of_park(&cx));
            assert!(!b.instead_of_park(&cx));
        }
    }

    #[test]
    fn a_socket_that_waited_before_the_spin_does_not_end_it() {
        let cx = Context::from_waker(Waker::noop());
        let mut earlier = spinner();
        assert!(earlier.instead_of_park(&cx));
        earlier.found();

        // `earlier` was the last to wait, but it is not waiting now.
        let mut a = spinner();
        assert!(a.instead_of_park(&cx));
        assert!(a.instead_of_park(&cx));
    }
}
