//! How long to keep looking for a message before going to sleep.
//!
//! Sleeping and being woken costs more than a round trip on a local socket,
//! and the sender pays for the wakeup inside its `send`.

use std::task::Context;
use std::time::{Duration, Instant};

/// Default time to look before sleeping. On a Unix socket REQ/REP round trip
/// (64 B, two cores) 5 µs and 20 µs measured alike, 7.0 µs against 10.0 without
/// spinning; 20 µs leaves room for a slower peer.
#[cfg(not(feature = "runtime-smol"))]
const DEFAULT_SPIN: Duration = Duration::from_micros(20);
/// smol wakes a sleeping thread through one reactor shared by the whole
/// process, re-armed on every wait, so a sleep costs far more there. One
/// sender feeding four workers gives each a batch every few dozen
/// microseconds: at 20 µs each worker slept between them and the pool ran at
/// half of 0.4's rate (5.4 M against 9.8 M msg/s at 64 B); at 100 µs it
/// passed it (10.3 M). compio and tokio lose at large frames with a longer
/// budget and keep 20 µs.
#[cfg(feature = "runtime-smol")]
const DEFAULT_SPIN: Duration = Duration::from_micros(100);

/// The spin budget.
///
/// `MONOCOQUE_SPIN_US` if set (0 disables), otherwise 20 µs on compio and
/// tokio and 100 µs on smol, or none when this thread may run on one CPU
/// only, where spinning only delays the peer it waits for.
#[must_use]
pub fn spin_budget() -> Duration {
    if let Some(us) = std::env::var("MONOCOQUE_SPIN_US")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Duration::from_micros(us);
    }
    if single_cpu() {
        Duration::ZERO
    } else {
        DEFAULT_SPIN
    }
}

/// `true` when this thread has one CPU to run on. On Linux the count honours
/// the affinity mask and the cgroup CPU quota.
///
/// Asked once per process, by whichever thread makes the first socket.
/// Finding the quota reads a file for each level of the cgroup tree, about
/// thirty system calls, which every new socket paid. A thread pinned to one
/// CPU inside a process that has several is not seen: `MONOCOQUE_SPIN_US=0`
/// is for that.
fn single_cpu() -> bool {
    static SINGLE_CPU: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SINGLE_CPU.get_or_init(|| std::thread::available_parallelism().map_or(true, |n| n.get() <= 1))
}

/// A receive about to sleep yields instead, for up to the budget.
///
/// The runtime turns over and polls its reactor meanwhile. For runtimes whose
/// loop this crate does not drive; compio's `block_on` does the same itself.
pub struct YieldSpin {
    budget: Duration,
    deadline: Option<Instant>,
    backoff: SpinBackoff,
}

impl Default for YieldSpin {
    fn default() -> Self {
        Self {
            budget: if cfg!(feature = "runtime-compio") {
                Duration::ZERO
            } else {
                spin_budget()
            },
            deadline: None,
            backoff: SpinBackoff::default(),
        }
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
        if self.budget.is_zero() {
            return false;
        }
        let me = std::ptr::from_ref(self).addr();
        let another_waited = LAST_WAITER.replace(me) != me;
        if another_waited && self.deadline.take().is_some() {
            self.backoff.thread_is_shared();
            return false;
        }
        let now = Instant::now();
        let deadline = match self.deadline {
            Some(d) => d,
            None if self.backoff.should_spin() => *self.deadline.insert(now + self.budget),
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
            budget: Duration::from_secs(60),
            deadline: None,
            backoff: SpinBackoff::default(),
        }
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
