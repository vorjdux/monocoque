//! How long to keep looking for a message before going to sleep.
//!
//! Sleeping and being woken costs more than a round trip on a local socket,
//! and the sender pays for the wakeup inside its `send`.

use std::task::Context;
use std::time::{Duration, Instant};

/// Default time to look before sleeping. On a Unix socket REQ/REP round trip
/// (64 B, two cores) 5 µs and 20 µs measured alike, 7.0 µs against 10.0 without
/// spinning; 20 µs leaves room for a slower peer.
const DEFAULT_SPIN: Duration = Duration::from_micros(20);

/// The spin budget: `MONOCOQUE_SPIN_US` if set (0 disables), otherwise
/// [`DEFAULT_SPIN`], or none when this thread may run on one CPU only, where
/// spinning only delays the peer it waits for.
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
fn single_cpu() -> bool {
    std::thread::available_parallelism().map_or(true, |n| n.get() <= 1)
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

impl YieldSpin {
    /// `true` if the caller should return `Pending` with its waker already
    /// woken, instead of parking.
    pub fn instead_of_park(&mut self, cx: &Context<'_>) -> bool {
        if self.budget.is_zero() {
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

    /// A spin found work.
    pub const fn hit(&mut self) {
        self.misses = 0;
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
