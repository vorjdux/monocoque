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
fn single_cpu() -> bool {
    std::thread::available_parallelism().map_or(true, |n| n.get() <= 1)
}

#[cfg(feature = "runtime-compio")]
thread_local! {
    static RECEIVED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Messages received on this thread so far. compio's `block_on` spins for
/// every socket on its thread, and reads how many messages a stretch
/// delivered from here.
#[cfg(feature = "runtime-compio")]
#[must_use]
pub fn received() -> u64 {
    RECEIVED.with(std::cell::Cell::get)
}

/// A receive about to sleep yields instead, for up to the budget.
///
/// The runtime turns over and polls its reactor meanwhile. For runtimes whose
/// loop this crate does not drive; compio's `block_on` does the same itself.
pub struct YieldSpin {
    budget: Duration,
    /// Whether the wait in progress began by spinning, and when it began if
    /// it is timed.
    wait: Option<(bool, Option<Instant>)>,
    /// Until when the wait in progress spins.
    deadline: Option<Instant>,
    /// The last wait, kept until the next one begins: how long it took and
    /// whether it spun.
    stretch: Option<(Duration, bool)>,
    /// Messages received since the last wait ended, with the one that ended
    /// it.
    messages: u32,
    choice: SpinChoice,
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
            wait: None,
            deadline: None,
            stretch: None,
            messages: 0,
            choice: SpinChoice::default(),
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
        let now = match self.wait {
            // A wait that sleeps reads no clock when polled again.
            Some((false, _)) => return false,
            Some((true, _)) => Instant::now(),
            None => {
                if let Some((waited, spun)) = self.stretch.take() {
                    self.choice.record_stretch(spun, waited, self.messages);
                }
                let (spin, timed) = self.choice.next_wait();
                if !spin && !timed {
                    self.wait = Some((false, None));
                    return false;
                }
                let now = Instant::now();
                self.wait = Some((spin, timed.then_some(now)));
                if !spin {
                    return false;
                }
                now
            }
        };
        // A spinning wait that ran out of budget parks, and spins again when
        // it is woken without a message: a wakeup says the peer is sending,
        // as when part of a large frame has arrived.
        let deadline = match self.deadline {
            Some(deadline) => deadline,
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

    /// A receive took a message that something else had queued, with `more`
    /// queued behind it.
    ///
    /// The spin in progress, if any, goes on into the next wait. Ending it
    /// here, so that the next wait spun for a budget of its own, made one
    /// sender feeding four workers 1024 B frames on smol a fifth slower
    /// (2.39 ms against 2.00 for 10 000).
    #[inline]
    pub fn found_queued(&mut self, more: u32) {
        self.count(more);
    }

    /// Whether the messages found now are being counted. When they are not,
    /// the caller need not work out how many are queued.
    #[inline]
    #[must_use]
    pub const fn counts(&self) -> bool {
        cfg!(feature = "runtime-compio")
            || self.stretch.is_some()
            || matches!(self.wait, Some((_, Some(_))))
    }

    /// A receive found a message after looking for one, with `more` queued
    /// behind it.
    ///
    /// The messages are counted here, a read at a time, and not as each is
    /// received: a receive that takes a queued message does nothing for the
    /// spin. Counting there cost a fan-in of 64 B messages 4 %.
    #[inline]
    pub fn found(&mut self, more: u32) {
        if self.deadline.take().is_some() {
            self.backoff.hit();
        }
        self.count(more);
    }

    #[inline]
    fn count(&mut self, more: u32) {
        let messages = more.saturating_add(1);
        #[cfg(feature = "runtime-compio")]
        RECEIVED.with(|n| n.set(n.get().wrapping_add(u64::from(messages))));
        if let Some((spun, began)) = self.wait.take() {
            self.stretch = began
                .map(|began| began.elapsed())
                .filter(|&waited| !SpinChoice::is_gap(waited, self.budget))
                .map(|waited| (waited, spun));
            self.messages = messages;
        } else {
            self.messages = self.messages.saturating_add(messages);
        }
    }
}

/// Stops spinning while it keeps finding nothing (an idle socket, or a peer
/// starved of CPU), and tries again after a while.
///
/// Without it, one sender feeding four workers on two cores took a quarter
/// longer on smol (2.57 ms against 1.94 for 10 000 frames of 1024 B): the
/// workers' spins took the CPU from the sender they were waiting for.
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

/// Chooses between spinning and sleeping by which has been moving messages
/// faster.
///
/// A spin that finds nothing is the obvious failure, but not the only one: a
/// spinning receiver keeps entering the kernel, and when it shares no cache
/// with its peer that slows the peer down. Measured on an EPYC 7232P (four L3
/// groups of two cores) with a REQ/REP round trip on tokio: two threads under
/// one L3 took 13.5 µs spinning and 20.9 µs sleeping; under two different L3s
/// they took 29.8 µs spinning and 21.8 µs sleeping. Every spin there found its
/// message, so counting misses saw nothing wrong.
///
/// How long one wait took does not settle it either, because the way this
/// side waits changes what arrives: a sleeping receiver makes the sender
/// wake it inside `send`, and a receiver that sleeps through a burst takes
/// the burst in one batch. A DEALER and a ROUTER exchanging pipelined 64 B
/// messages run a third faster when they sleep (2.29 ms against 3.05 for a
/// batch on smol), though each of their waits is longer. So this divides the
/// time a wait took by the messages received from then until the next wait:
/// the time spent waiting per message, which is what the way of waiting can
/// change. Time spent outside a wait is the same either way and is left out,
/// so an application that is slow to come back for more does not count
/// against whichever way was in use.
///
/// It keeps that figure for waits that spun and for waits that slept. Most
/// waits use whichever has been faster; a few in each cycle use the other, so
/// both stay current when the load or the placement changes.
///
/// This decides whether a wait may spin at all. [`SpinBackoff`] still pauses
/// the spinning of a socket whose spins find nothing.
pub struct SpinChoice {
    /// Time waited per message by waits that spun.
    spun: WaitAverage,
    /// Time waited per message by waits that slept.
    slept: WaitAverage,
    turn: u32,
    /// Waits in the current cycle.
    cycle: u32,
    /// What the last cycle preferred.
    preferred: bool,
    /// What the figures prefer now: `true` for spinning.
    prefers: bool,
}

impl Default for SpinChoice {
    fn default() -> Self {
        Self {
            spun: WaitAverage::default(),
            slept: WaitAverage::default(),
            turn: 0,
            cycle: Self::CYCLE,
            preferred: true,
            prefers: true,
        }
    }
}

impl SpinChoice {
    /// Waits in the shortest cycle.
    const CYCLE: u32 = 256;
    /// Waits in the longest cycle. Each trial costs the difference between
    /// the two ways: at 4 in 256 a Unix socket REQ/REP round trip on tokio
    /// that spinning shortens to 3.0 µs was 3 % slower than always spinning.
    /// So a cycle that ends preferring what the one before it preferred is
    /// followed by one twice as long, up to this, and a change of preference
    /// starts again from the shortest.
    const LONGEST_CYCLE: u32 = 4096;
    /// Waits at the start of each cycle that try the way not preferred.
    const TRIAL: u32 = 4;
    /// Waits at the start of a cycle that are all timed, and that a cycle
    /// must have run before a change of preference may start another, so
    /// that trials stay under an eighth of the waits.
    const RECHECK: u32 = Self::TRIAL * 8;
    /// Of the waits after those, one in this many is timed.
    const TIMED: u32 = 8;

    /// Whether the wait about to begin should spin, and whether it should be
    /// timed.
    ///
    /// Timing a wait reads the clock once more than waiting does, and a Unix
    /// socket REQ/REP round trip has two waits in its 3 µs: timing every one
    /// and weighing the two ways before each cost it about 2 % on tokio. So
    /// the first waits of a cycle are timed, the trials among them, and one
    /// in [`Self::TIMED`] of the rest.
    pub fn next_wait(&mut self) -> (bool, bool) {
        let prefers = self.prefers;
        let trial = self.turn < Self::TRIAL;
        let timed = self.turn < Self::RECHECK || self.turn.is_multiple_of(Self::TIMED);
        self.turn += 1;
        if self.turn >= self.cycle {
            self.turn = 0;
            self.cycle = if prefers == self.preferred {
                (self.cycle * 2).min(Self::LONGEST_CYCLE)
            } else {
                Self::CYCLE
            };
            self.preferred = prefers;
        }
        (prefers != trial, timed)
    }

    /// `true` if a wait of `waited`, with a spin of `budget`, was a gap in
    /// the traffic, which should not be recorded.
    ///
    /// A wait longer than the budget had its message out of a spin's reach,
    /// so it says nothing about which way is faster, and it would swamp the
    /// time per message of the waits that a spin can cover. With the gaps
    /// between bursts counted, one sender feeding four workers on smol
    /// stopped spinning inside the bursts and took twice as long (1.20 ms
    /// against 0.60 for 10 000 messages of 64 B). With stalls of 30 to 60 µs
    /// counted against a 20 µs spin, a Unix socket REQ/REP on tokio took
    /// them for spinning being slow and slept through 1 % of its waits,
    /// which made its round trip 3 % longer.
    #[must_use]
    pub fn is_gap(waited: Duration, budget: Duration) -> bool {
        waited > budget
    }

    /// A wait that began by spinning (or not) took `waited`, and `messages`
    /// were received from then until the next wait.
    pub fn record_stretch(&mut self, spun: bool, waited: Duration, messages: u32) {
        if messages == 0 {
            return;
        }
        if spun {
            self.spun.add(nanos(waited), messages);
        } else {
            self.slept.add(nanos(waited), messages);
        }
        let prefers = self.prefers_spin();
        if prefers != self.prefers {
            self.prefers = prefers;
            // A change of mind is checked at once, by starting a cycle: its
            // trials go the way just given up, and the waits after them are
            // all timed. Left until the cycle ended, a mistaken change kept
            // a socket waiting the slower way for hundreds of waits.
            if self.turn >= Self::RECHECK {
                self.turn = 0;
                self.cycle = Self::CYCLE;
                self.preferred = prefers;
            }
        }
    }

    /// Spinning is kept unless sleeping waits a fifth less per message. A
    /// near tie goes to spinning, as on a socket that never measured, and so
    /// does a side that has not yet tried both: the figures are noisy in
    /// bursts, and with a tenth as the margin a fan-in of four senders kept
    /// changing its mind and ran 6 to 12 % slower on smol.
    fn prefers_spin(&self) -> bool {
        if self.spun.messages == 0 || self.slept.messages == 0 {
            return true;
        }
        let spun = u128::from(self.spun.nanos) * u128::from(self.slept.messages);
        let slept = u128::from(self.slept.nanos) * u128::from(self.spun.messages);
        spun * 4 < slept * 5
    }
}

fn nanos(time: Duration) -> u64 {
    u64::try_from(time.as_nanos()).unwrap_or(u64::MAX)
}

/// Time waited per message over recent waits, in nanoseconds.
///
/// The time and the messages are summed apart and then divided, so a wait
/// counts for as many messages as it brought. An average of each wait's own
/// time per message is ruled by the few waits that brought one message
/// slowly: in bursts it read 23 µs per message where the stream ran at 0.3.
#[derive(Default)]
struct WaitAverage {
    nanos: u64,
    /// Sixteen times the messages, so the decay loses little.
    messages: u64,
}

impl WaitAverage {
    /// Each wait takes an eighth off what the ones before it weigh.
    fn add(&mut self, nanos: u64, messages: u32) {
        // A wait counts for at most twice the average per message, so one
        // stall does not change the preference.
        let most = u128::from(self.nanos) * 32 * u128::from(messages);
        let nanos = if self.messages != 0 && u128::from(nanos) * u128::from(self.messages) > most {
            u64::try_from(most / u128::from(self.messages)).unwrap_or(u64::MAX)
        } else {
            nanos
        };
        self.nanos = (self.nanos - self.nanos / 8).saturating_add(nanos);
        self.messages = self.messages - self.messages / 8 + u64::from(messages) * 16;
    }

    #[cfg(test)]
    fn nanos(&self) -> Option<u64> {
        (self.messages != 0).then(|| self.nanos.saturating_mul(16) / self.messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_micros(20);
    /// Enough waits for several cycles of any length.
    const WAITS: u32 = SpinChoice::LONGEST_CYCLE * 4;

    /// How one way of waiting behaves: how long a wait lasts, in
    /// microseconds, and how many messages come before the next wait.
    #[derive(Clone, Copy)]
    struct Way {
        wait: u64,
        messages: u32,
    }

    /// A wait of `micros` that brings one message.
    const fn each(micros: u64) -> Way {
        Way {
            wait: micros,
            messages: 1,
        }
    }

    /// Run `WAITS` waits and return how many of the second half spun.
    fn settle(choice: &mut SpinChoice, spun: Way, slept: Way) -> u32 {
        let mut spins = 0;
        for turn in 0..WAITS {
            let (spin, _) = choice.next_wait();
            if turn >= WAITS / 2 {
                spins += u32::from(spin);
            }
            let way = if spin { spun } else { slept };
            let waited = Duration::from_micros(way.wait);
            if !SpinChoice::is_gap(waited, BUDGET) {
                choice.record_stretch(spin, waited, way.messages);
            }
        }
        spins
    }

    /// Of the waits counted, at most this many are trials.
    const TRIALS: u32 = (WAITS / 2 / SpinChoice::CYCLE) * SpinChoice::TRIAL;
    const MOSTLY: u32 = WAITS / 2 - TRIALS;

    #[test]
    fn spins_when_spinning_gets_the_message_sooner() {
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, each(7), each(11)) >= MOSTLY);
    }

    #[test]
    fn sleeps_when_spinning_finds_the_message_but_later() {
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, each(16), each(11)) <= TRIALS);
    }

    #[test]
    fn sleeps_when_sleeping_is_clearly_faster() {
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, each(15), each(10)) <= TRIALS);
    }

    #[test]
    fn a_tie_spins() {
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, each(5), each(5)) >= MOSTLY);
    }

    #[test]
    fn a_near_tie_spins() {
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, each(10), each(9)) >= MOSTLY);
    }

    #[test]
    fn spins_when_sleeping_waits_no_longer_but_slows_the_sender() {
        // The wait is the same either way; the sender pays for the wakeup,
        // so fewer messages arrive before the next wait.
        let spun = Way {
            wait: 15,
            messages: 4,
        };
        let slept = Way {
            wait: 15,
            messages: 2,
        };
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, spun, slept) >= MOSTLY);
    }

    #[test]
    fn sleeps_when_a_longer_wait_takes_a_whole_batch() {
        let spun = Way {
            wait: 3,
            messages: 1,
        };
        let slept = Way {
            wait: 12,
            messages: 8,
        };
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, spun, slept) <= TRIALS);
    }

    #[test]
    fn gaps_in_the_traffic_do_not_count() {
        // Every wait is far longer than a spin, whichever way it began, so
        // nothing is learned and it goes on as a socket that never measured.
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, each(500), each(400)) >= MOSTLY);
    }

    #[test]
    fn a_wait_counts_for_the_messages_it_brought() {
        // One slow wait for one message among many that bring a batch.
        let mut average = WaitAverage::default();
        average.add(30_000, 1);
        for _ in 0..3 {
            average.add(10_000, 100);
        }
        assert!(average.nanos().unwrap() < 300);
    }

    #[test]
    fn spinning_pauses_after_spins_that_find_nothing() {
        let mut backoff = SpinBackoff::default();
        for _ in 0..SpinBackoff::MAX_MISSES {
            assert!(backoff.should_spin());
            backoff.miss();
        }
        for _ in 0..SpinBackoff::PAUSE {
            assert!(!backoff.should_spin());
        }
        assert!(backoff.should_spin());
    }

    #[test]
    fn follows_a_change_in_which_is_faster() {
        let mut choice = SpinChoice::default();
        assert!(settle(&mut choice, each(7), each(11)) >= MOSTLY);
        assert!(settle(&mut choice, each(16), each(11)) <= TRIALS);
        assert!(settle(&mut choice, each(7), each(11)) >= MOSTLY);
    }

    #[test]
    fn trials_grow_rarer_while_the_preference_holds() {
        let mut choice = SpinChoice::default();
        settle(&mut choice, each(7), each(11));
        assert_eq!(choice.cycle, SpinChoice::LONGEST_CYCLE);
        settle(&mut choice, each(16), each(11));
        assert!(!choice.preferred);
    }
}
