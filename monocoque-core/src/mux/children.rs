//! The futures a socket owns, and the wakers that route their wakeups.
//!
//! Every long-lived piece of per-socket work is a child future here: a peer's
//! reader and writer loops, a listener's accept loop, a connector, a handshake,
//! the heartbeat ticker. Nothing is spawned per peer. Whichever task touches the
//! socket polls the children that have woken: the user task inside `recv` or
//! `send`, or the socket's driver task when the user is elsewhere.
//!
//! A child's waker records the child as woken, then wakes the user task if one
//! is parked on the socket and the driver otherwise. That keeps the latency
//! path inline: a read completing while the user waits in `recv` wakes the user
//! directly, and the user's poll runs the read loop that produces the message.
//! No other task is scheduled in between.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Wake, Waker};

/// A child future in a [`Children`] set.
///
/// It names a slot and which occupant of that slot it is. Slots are reused,
/// so an id kept after its child finished on its own (a reader that hit EOF)
/// must not reach the slot's next occupant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildId {
    slot: usize,
    generation: u32,
}

#[derive(Default)]
struct WakeState {
    woken: Vec<ChildId>,
    user: Option<Waker>,
    driver: Option<Waker>,
}

/// Wake routing shared by every child waker of one socket.
///
/// The two flags mirror state kept under the lock (`woken` non-empty, `user`
/// set) so the hot paths that usually find nothing to do skip the lock. They
/// are only written under the lock, which orders them with the state they
/// describe.
#[derive(Default)]
pub struct Wakeups {
    state: Mutex<WakeState>,
    pending: AtomicBool,
    user_parked: AtomicBool,
    active: AtomicBool,
}

// The guards here are held across the flag stores on purpose: a flag written
// after the lock is released could be observed out of step with the state it
// mirrors.
#[allow(clippy::significant_drop_tightening)]
impl Wakeups {
    /// The user task is inside a socket operation, which drains woken
    /// children itself before returning: wakeups only need queueing.
    pub fn enter(&self) {
        self.active.store(true, Ordering::Release);
    }

    /// The operation is returning. Anything still queued goes to the driver
    /// unless the user parked for it.
    pub fn exit(&self) {
        self.active.store(false, Ordering::Release);
        if !self.pending.load(Ordering::Acquire) {
            return;
        }
        // A wakeup raised after the operation's last drain (possibly from
        // another thread's reactor) was only queued; deliver it now.
        let target = {
            let mut s = self.lock();
            if s.woken.is_empty() {
                return;
            }
            let user = s.user.take();
            if user.is_some() {
                self.user_parked.store(false, Ordering::Release);
            }
            user.or_else(|| s.driver.clone())
        };
        if let Some(w) = target {
            w.wake();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WakeState> {
        // A panic while holding this lock leaves only a list of indices behind;
        // recovering it is always sound.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Park the user task: the next wakeup goes to `waker`.
    pub fn park_user(&self, waker: &Waker) {
        let mut s = self.lock();
        match &s.user {
            Some(w) if w.will_wake(waker) => {}
            _ => s.user = Some(waker.clone()),
        }
        self.user_parked.store(true, Ordering::Release);
    }

    /// The user task is no longer parked; wakeups go to the driver again.
    pub fn unpark_user(&self) {
        if !self.user_parked.load(Ordering::Acquire) && !self.pending.load(Ordering::Acquire) {
            return;
        }
        let pending = {
            let mut s = self.lock();
            s.user = None;
            self.user_parked.store(false, Ordering::Release);
            !s.woken.is_empty()
        };
        // Work queued for a user who has since left must not be stranded.
        if pending {
            self.wake_driver();
        }
    }

    /// Record the driver's waker.
    pub fn set_driver(&self, waker: &Waker) {
        let mut s = self.lock();
        match &s.driver {
            Some(w) if w.will_wake(waker) => {}
            _ => s.driver = Some(waker.clone()),
        }
    }

    /// Forget the driver (it has finished).
    pub fn clear_driver(&self) {
        self.lock().driver = None;
    }

    /// Wake the driver task.
    pub fn wake_driver(&self) {
        let w = self.lock().driver.clone();
        if let Some(w) = w {
            w.wake();
        }
    }

    /// Wake the parked user task, if any. Used when the driver produced
    /// something a parked user is waiting for.
    pub fn wake_user(&self) {
        if !self.user_parked.load(Ordering::Acquire) {
            return;
        }
        let w = {
            let mut s = self.lock();
            self.user_parked.store(false, Ordering::Release);
            s.user.take()
        };
        if let Some(w) = w {
            w.wake();
        }
    }

    fn notify(&self, id: ChildId) {
        let target = {
            let mut s = self.lock();
            s.woken.push(id);
            self.pending.store(true, Ordering::Release);
            if self.active.load(Ordering::Acquire) {
                return;
            }
            let user = s.user.take();
            if user.is_some() {
                self.user_parked.store(false, Ordering::Release);
            }
            user.or_else(|| s.driver.clone())
        };
        if let Some(w) = target {
            w.wake();
        }
    }

    fn take_woken(&self, out: &mut Vec<ChildId>) {
        if !self.pending.load(Ordering::Acquire) {
            return;
        }
        let mut s = self.lock();
        std::mem::swap(&mut s.woken, out);
        self.pending.store(false, Ordering::Release);
    }

    /// `true` if some child has woken and not been polled yet.
    pub fn has_woken(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

struct ChildWaker {
    id: ChildId,
    queued: AtomicBool,
    wakeups: Arc<Wakeups>,
}

impl Wake for ChildWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if !self.queued.swap(true, Ordering::AcqRel) {
            self.wakeups.notify(self.id);
        }
    }
}

struct Child {
    fut: Pin<Box<dyn Future<Output = ()>>>,
    flag: Arc<ChildWaker>,
    waker: Waker,
}

/// The set of child futures owned by one socket.
pub struct Children {
    slots: Vec<Option<Child>>,
    /// The current occupant's generation, per slot.
    generations: Vec<u32>,
    free: Vec<usize>,
    wakeups: Arc<Wakeups>,
    scratch: Vec<ChildId>,
}

impl Children {
    /// An empty set routing wakeups through `wakeups`.
    pub fn new(wakeups: Arc<Wakeups>) -> Self {
        Self {
            slots: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            wakeups,
            scratch: Vec::new(),
        }
    }

    /// The live child `id` names, if it is still the slot's occupant.
    fn get_mut(&mut self, id: ChildId) -> Option<&mut Child> {
        if self.generations.get(id.slot) != Some(&id.generation) {
            return None;
        }
        self.slots.get_mut(id.slot).and_then(Option::as_mut)
    }

    /// The wake routing this set reports to.
    pub const fn wakeups(&self) -> &Arc<Wakeups> {
        &self.wakeups
    }

    /// Add a child. It is polled on the next [`poll_woken`](Self::poll_woken).
    pub fn spawn(&mut self, fut: impl Future<Output = ()> + 'static) -> ChildId {
        let id = if let Some(slot) = self.free.pop() {
            self.generations[slot] = self.generations[slot].wrapping_add(1);
            ChildId {
                slot,
                generation: self.generations[slot],
            }
        } else {
            self.slots.push(None);
            self.generations.push(0);
            ChildId {
                slot: self.slots.len() - 1,
                generation: 0,
            }
        };
        let flag = Arc::new(ChildWaker {
            id,
            queued: AtomicBool::new(false),
            wakeups: Arc::clone(&self.wakeups),
        });
        let waker = Waker::from(Arc::clone(&flag));
        let child = self.slots[id.slot].insert(Child {
            fut: Box::pin(fut),
            flag,
            waker,
        });
        child.waker.wake_by_ref();
        id
    }

    /// Drop a child without polling it again. An id whose child already
    /// finished is ignored, even if its slot has a new occupant.
    pub fn cancel(&mut self, id: ChildId) {
        if self.get_mut(id).is_some() {
            self.slots[id.slot] = None;
            self.free.push(id.slot);
        }
    }

    /// Number of live children.
    pub fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    /// `true` when no child is alive.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Poll every child that has woken since the last call. Returns `true` if
    /// any child was polled.
    pub fn poll_woken(&mut self) -> bool {
        let mut any = false;
        loop {
            let mut woken = std::mem::take(&mut self.scratch);
            self.wakeups.take_woken(&mut woken);
            if woken.is_empty() {
                self.scratch = woken;
                return any;
            }
            any = true;
            for &id in &woken {
                let done = match self.get_mut(id) {
                    Some(child) => {
                        // Clear first, so a wakeup raised during this poll
                        // queues the child again.
                        child.flag.queued.store(false, Ordering::Release);
                        let mut cx = Context::from_waker(&child.waker);
                        child.fut.as_mut().poll(&mut cx).is_ready()
                    }
                    None => false,
                };
                if done {
                    self.cancel(id);
                }
            }
            woken.clear();
            self.scratch = woken;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::atomic::AtomicUsize;
    use std::task::Poll;

    struct Count(AtomicUsize);
    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counter() -> (Arc<Count>, Waker) {
        let c = Arc::new(Count(AtomicUsize::new(0)));
        (Arc::clone(&c), Waker::from(c))
    }

    #[test]
    fn spawned_child_runs_on_first_poll_and_finishes() {
        let mut set = Children::new(Arc::default());
        let ran = Rc::new(Cell::new(false));
        let r = Rc::clone(&ran);
        set.spawn(async move { r.set(true) });
        assert_eq!(set.len(), 1);
        assert!(set.poll_woken());
        assert!(ran.get());
        assert!(set.is_empty());
    }

    #[test]
    fn stale_id_of_a_finished_child_does_not_cancel_the_slots_next_occupant() {
        let mut set = Children::new(Arc::default());
        // Finishes on its first poll, freeing its slot, like a reader at EOF.
        let done = set.spawn(async {});
        set.poll_woken();
        assert!(set.is_empty());

        let ran = Rc::new(Cell::new(false));
        let r = Rc::clone(&ran);
        let next = set.spawn(async move { r.set(true) });
        assert_ne!(done, next);
        // The owner of the finished child cancels its id late.
        set.cancel(done);
        assert_eq!(set.len(), 1, "the new occupant must survive");
        set.poll_woken();
        assert!(ran.get());
    }

    #[test]
    fn wake_goes_to_parked_user_then_to_driver() {
        let wakeups: Arc<Wakeups> = Arc::default();
        let mut set = Children::new(Arc::clone(&wakeups));
        let (user_count, user) = counter();
        let (driver_count, driver) = counter();
        wakeups.set_driver(&driver);

        // A child that parks its waker and waits for it to be fired.
        let slot: Rc<RefCell<Option<Waker>>> = Rc::default();
        let fired = Rc::new(Cell::new(false));
        let (s, f) = (Rc::clone(&slot), Rc::clone(&fired));
        set.spawn(std::future::poll_fn(move |cx| {
            if f.get() {
                Poll::Ready(())
            } else {
                *s.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        }));
        set.poll_woken();
        // Spawning woke the driver (nobody was parked).
        assert_eq!(driver_count.0.load(Ordering::SeqCst), 1);

        wakeups.park_user(&user);
        slot.borrow().as_ref().unwrap().wake_by_ref();
        assert_eq!(user_count.0.load(Ordering::SeqCst), 1);
        assert_eq!(driver_count.0.load(Ordering::SeqCst), 1);

        // A second wake before the poll is coalesced.
        slot.borrow().as_ref().unwrap().wake_by_ref();
        assert_eq!(user_count.0.load(Ordering::SeqCst), 1);

        fired.set(true);
        set.poll_woken();
        assert!(set.is_empty());
    }

    #[test]
    fn leaving_user_hands_pending_work_to_the_driver() {
        let wakeups: Arc<Wakeups> = Arc::default();
        let mut set = Children::new(Arc::clone(&wakeups));
        let (_, user) = counter();
        let (driver_count, driver) = counter();
        wakeups.set_driver(&driver);
        wakeups.park_user(&user);
        set.spawn(async {});
        assert!(wakeups.has_woken());
        wakeups.unpark_user();
        assert_eq!(driver_count.0.load(Ordering::SeqCst), 1);
    }
}
