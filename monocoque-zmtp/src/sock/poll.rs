//! Wait on many sockets at once (`zmq_poll`).
//!
//! Build a slice of [`PollItem`]s, each naming a socket and the events it is
//! interested in, and call [`poll`]. It returns how many items are ready and
//! sets each item's [`revents`](PollItem::revents). Sockets of any kind, and
//! STREAM sockets, mix freely in one slice.
//!
//! ```no_run
//! # async fn demo() -> monocoque_core::error::Result<()> {
//! use monocoque_zmtp::sock::kinds::{PullSocket, SubSocket};
//! use monocoque_zmtp::sock::poll::{PollEvents, PollItem, poll};
//!
//! let mut jobs = PullSocket::new();
//! let mut control = SubSocket::new();
//! loop {
//!     let mut items = [
//!         PollItem::new(&jobs, PollEvents::POLLIN),
//!         PollItem::new(&control, PollEvents::POLLIN),
//!     ];
//!     poll(&mut items, None).await?;
//!     let (job, ctl) = (items[0].is_readable(), items[1].is_readable());
//!     if job {
//!         let _msg = jobs.recv().await?;
//!     }
//!     if ctl {
//!         let _cmd = control.recv().await?;
//!     }
//! }
//! # }
//! ```
//!
//! Readiness comes from each socket's core, the same state its own receive
//! and send look at, so a ready item stays ready until the caller acts on it.
//! Nothing is received or sent by a poll. When nothing is ready the poll parks
//! its task on every item's socket, so whichever becomes ready first wakes it;
//! when it returns, or is dropped mid-wait, it hands every socket back to its
//! own background driver. A poll does not allocate.

use super::stream::StreamSocket;
use super::{Kind, Socket};
use monocoque_core::error::Result;
use monocoque_core::mux::{Hooks, Protocol, Shared, SocketCore};
use std::future::Future;
use std::ops::{BitAnd, BitOr, BitOrAssign};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// A set of poll events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PollEvents(u8);

impl PollEvents {
    /// No events.
    pub const NONE: Self = Self(0);
    /// A receive would return a message without waiting (`ZMQ_POLLIN`).
    pub const POLLIN: Self = Self(1);
    /// A send would be accepted without waiting (`ZMQ_POLLOUT`).
    pub const POLLOUT: Self = Self(2);

    /// `true` if no event is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `true` if every event in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for PollEvents {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for PollEvents {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for PollEvents {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Something [`poll`] can wait on. Every [`Socket`] and [`StreamSocket`]
/// implements it.
pub trait Pollable {
    /// The events of `interest` that are ready now. With `cx`, when none is
    /// ready, also arrange for `cx` to be woken once one may be; the caller
    /// must then call [`release`](Self::release) when it stops waiting. Never
    /// takes or sends a message.
    fn poll_events(&self, interest: PollEvents, cx: Option<&mut Context<'_>>) -> PollEvents;

    /// Stop routing this socket's wakeups to the polling task.
    fn release(&self);
}

/// Shared body of every `poll_events`: drive the core, check, and park on it
/// if nothing is ready.
fn poll_core<P: Protocol, H: Hooks>(
    core: &Shared<P, H>,
    interest: PollEvents,
    mut cx: Option<&mut Context<'_>>,
    mut check: impl FnMut(&mut SocketCore<P, H>, PollEvents) -> PollEvents,
) -> PollEvents {
    core.borrow().wakeups().enter();
    let ready = {
        let mut c = core.borrow_mut();
        loop {
            let ready = check(&mut c, interest);
            if !ready.is_empty() {
                break ready;
            }
            let Some(cx) = cx.as_deref_mut() else {
                break ready;
            };
            if interest.contains(PollEvents::POLLOUT) {
                c.watch_send_room(cx);
            }
            let quiet = c.park_poll(cx);
            // Look once more with the waker in place. The checks drive the
            // core, so work they absorbed after the first look (a message
            // landing while the writable check ran) is seen here, and anything
            // later wakes the waker just parked.
            let ready = check(&mut c, interest);
            if !ready.is_empty() || quiet {
                break ready;
            }
        }
    };
    core.borrow().wakeups().exit();
    ready
}

fn release_core<P: Protocol, H: Hooks>(core: &Shared<P, H>) {
    if let Ok(c) = core.try_borrow() {
        c.wakeups().unpark_user();
    }
}

impl<K: Kind> Pollable for Socket<K> {
    fn poll_events(&self, interest: PollEvents, cx: Option<&mut Context<'_>>) -> PollEvents {
        let ext = &self.ext;
        poll_core(&self.core, interest, cx, |c, want| {
            let mut ready = PollEvents::NONE;
            if want.contains(PollEvents::POLLIN) && K::readable(ext, c) {
                ready |= PollEvents::POLLIN;
            }
            if want.contains(PollEvents::POLLOUT) && K::writable(ext, c) {
                ready |= PollEvents::POLLOUT;
            }
            ready
        })
    }

    fn release(&self) {
        release_core(&self.core);
    }
}

impl Pollable for StreamSocket {
    fn poll_events(&self, interest: PollEvents, cx: Option<&mut Context<'_>>) -> PollEvents {
        poll_core(&self.core, interest, cx, |c, want| {
            let mut ready = PollEvents::NONE;
            if want.contains(PollEvents::POLLIN)
                && (c.peek_ready() || super::stream::has_notification(c))
            {
                ready |= PollEvents::POLLIN;
            }
            // Like libzmq, STREAM always reports POLLOUT: a send to a full or
            // unknown peer fails at once instead of waiting.
            if want.contains(PollEvents::POLLOUT) {
                ready |= PollEvents::POLLOUT;
            }
            ready
        })
    }

    fn release(&self) {
        release_core(&self.core);
    }
}

/// One socket and the events to wait for (`zmq_pollitem_t`).
pub struct PollItem<'a> {
    socket: &'a dyn Pollable,
    events: PollEvents,
    revents: PollEvents,
}

impl<'a> PollItem<'a> {
    /// Watch `socket` for `events`.
    pub fn new(socket: &'a dyn Pollable, events: PollEvents) -> Self {
        Self {
            socket,
            events,
            revents: PollEvents::NONE,
        }
    }

    /// The events asked for.
    #[must_use]
    pub const fn events(&self) -> PollEvents {
        self.events
    }

    /// The events found ready by the last [`poll`].
    #[must_use]
    pub const fn revents(&self) -> PollEvents {
        self.revents
    }

    /// `true` if the last poll found the socket readable.
    #[must_use]
    pub const fn is_readable(&self) -> bool {
        self.revents.contains(PollEvents::POLLIN)
    }

    /// `true` if the last poll found the socket writable.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.revents.contains(PollEvents::POLLOUT)
    }
}

/// Check every item once, filling `revents`; with `cx`, park on each socket
/// that has nothing ready. Returns the number of ready items.
fn check_all(items: &mut [PollItem<'_>], mut cx: Option<&mut Context<'_>>) -> usize {
    let mut ready = 0;
    for item in items.iter_mut() {
        item.revents = if item.events.is_empty() {
            PollEvents::NONE
        } else {
            item.socket.poll_events(item.events, cx.as_deref_mut())
        };
        if !item.revents.is_empty() {
            ready += 1;
        }
    }
    ready
}

fn release_all(items: &[PollItem<'_>]) {
    for item in items {
        if !item.events.is_empty() {
            item.socket.release();
        }
    }
}

/// The wait behind [`poll`]. Dropping it hands every socket back to its
/// driver, so a poll raced in a `select` or cut short by a timeout leaves
/// nothing stranded.
struct PollWait<'i, 'a> {
    items: &'i mut [PollItem<'a>],
    parked: bool,
}

impl Future for PollWait<'_, '_> {
    type Output = usize;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
        let this = &mut *self;
        let ready = check_all(this.items, Some(cx));
        this.parked = true;
        if ready > 0 {
            release_all(this.items);
            this.parked = false;
            Poll::Ready(ready)
        } else {
            Poll::Pending
        }
    }
}

impl Drop for PollWait<'_, '_> {
    fn drop(&mut self) {
        if self.parked {
            release_all(self.items);
        }
    }
}

/// Wait until at least one item is ready, then report every ready item
/// (`zmq_poll`).
///
/// Sets each item's [`revents`](PollItem::revents) and returns how many items
/// have a non-empty one. `timeout` of `None` waits forever; `Some(ZERO)`
/// checks once without waiting; otherwise the wait ends after `timeout` with
/// `Ok(0)`. Items with no `events` are skipped. Nothing is received or sent,
/// and dropping the returned future mid-wait is safe.
pub async fn poll(items: &mut [PollItem<'_>], timeout: Option<Duration>) -> Result<usize> {
    if timeout.is_some_and(|d| d.is_zero()) {
        return Ok(check_all(items, None));
    }
    let wait = PollWait {
        items,
        parked: false,
    };
    match timeout {
        None => Ok(wait.await),
        // On expiry the wait is dropped, which released every socket, and
        // every item's `revents` is empty from its last check.
        Some(d) => Ok(monocoque_core::rt::timeout(d, wait).await.unwrap_or(0)),
    }
}
