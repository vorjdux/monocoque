//! `LocalQueue`: a same-thread, single-producer single-consumer bounded queue.
//!
//! The inproc transport between two sockets on one thread, and every queue
//! between a peer and its socket, is one of these. It is an `Rc<RefCell<..>>`
//! around a `VecDeque` with one waker per side, so neither send nor receive
//! touches an atomic. The two ends are `!Send` by construction.
//!
//! Capacity is the high-water mark. `0` means unbounded, the deliberate
//! user-selected exception libzmq also makes for `hwm == 0`.
//!
//! - [`LocalSender::try_send`] never waits: at capacity it hands the value back
//!   so the caller can apply its socket type's policy (PUB drops).
//! - [`LocalSender::send`] waits at capacity until the receiver makes room,
//!   which is the blocking HWM behavior DEALER, PUSH and REQ need.
//! - A conflating queue holds at most one value and a send replaces it, so the
//!   receiver only ever sees the newest message (`ZMQ_CONFLATE`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

struct Inner<T> {
    buf: VecDeque<T>,
    cap: usize,
    conflate: bool,
    sender_alive: bool,
    receiver_alive: bool,
    recv_waker: Option<Waker>,
    send_waker: Option<Waker>,
}

impl<T> Inner<T> {
    fn full(&self) -> bool {
        !self.conflate && self.cap != 0 && self.buf.len() >= self.cap
    }

    fn wake_receiver(&mut self) {
        if let Some(w) = self.recv_waker.take() {
            w.wake();
        }
    }

    fn wake_sender(&mut self) {
        if let Some(w) = self.send_waker.take() {
            w.wake();
        }
    }
}

/// Why a [`LocalSender::try_send`] did not enqueue its value.
#[derive(PartialEq, Eq)]
pub enum TrySendError<T> {
    /// The queue is at its high-water mark.
    Full(T),
    /// The receiving end is gone.
    Closed(T),
}

impl<T> TrySendError<T> {
    /// Recover the value that was not sent.
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(v) | Self::Closed(v) => v,
        }
    }
}

impl<T> fmt::Debug for TrySendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(_) => f.write_str("Full(..)"),
            Self::Closed(_) => f.write_str("Closed(..)"),
        }
    }
}

/// Sending half of a [`local_queue`].
pub struct LocalSender<T> {
    inner: Rc<RefCell<Inner<T>>>,
}

/// Receiving half of a [`local_queue`].
pub struct LocalReceiver<T> {
    inner: Rc<RefCell<Inner<T>>>,
}

/// Create a queue holding up to `cap` values (`0` for unbounded).
#[must_use]
pub fn local_queue<T>(cap: usize) -> (LocalSender<T>, LocalReceiver<T>) {
    new_queue(cap, false)
}

/// Create a conflating queue: it holds one value and each send replaces it.
#[must_use]
pub fn conflating_queue<T>() -> (LocalSender<T>, LocalReceiver<T>) {
    new_queue(1, true)
}

fn new_queue<T>(cap: usize, conflate: bool) -> (LocalSender<T>, LocalReceiver<T>) {
    let inner = Rc::new(RefCell::new(Inner {
        buf: VecDeque::new(),
        cap,
        conflate,
        sender_alive: true,
        receiver_alive: true,
        recv_waker: None,
        send_waker: None,
    }));
    (
        LocalSender {
            inner: Rc::clone(&inner),
        },
        LocalReceiver { inner },
    )
}

impl<T> LocalSender<T> {
    /// Enqueue `value` without waiting.
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        let mut inner = self.inner.borrow_mut();
        if !inner.receiver_alive {
            return Err(TrySendError::Closed(value));
        }
        if inner.conflate {
            inner.buf.clear();
        } else if inner.full() {
            return Err(TrySendError::Full(value));
        }
        inner.buf.push_back(value);
        inner.wake_receiver();
        Ok(())
    }

    /// Wait until the queue has room (or the receiver is gone).
    ///
    /// Returns `Ready(false)` when the receiver has been dropped.
    pub fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<bool> {
        let mut inner = self.inner.borrow_mut();
        if !inner.receiver_alive {
            return Poll::Ready(false);
        }
        if inner.full() {
            inner.send_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        Poll::Ready(true)
    }

    /// Enqueue `value`, waiting while the queue is at its high-water mark.
    pub async fn send(&self, value: T) -> Result<(), T> {
        if !poll_fn(|cx| self.poll_ready(cx)).await {
            return Err(value);
        }
        self.try_send(value).map_err(TrySendError::into_inner)
    }

    /// Values currently queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.borrow().buf.len()
    }

    /// `true` when nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `true` while the receiving end exists.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.inner.borrow().receiver_alive
    }
}

impl<T> LocalReceiver<T> {
    /// Take the next value if one is queued.
    ///
    /// `Ok(None)` means empty for now; `Err(())` means empty and the sender is
    /// gone, so nothing will ever arrive.
    #[allow(clippy::result_unit_err)]
    pub fn try_recv(&self) -> Result<Option<T>, ()> {
        let mut inner = self.inner.borrow_mut();
        match inner.buf.pop_front() {
            Some(v) => {
                inner.wake_sender();
                Ok(Some(v))
            }
            None if inner.sender_alive => Ok(None),
            None => Err(()),
        }
    }

    /// Poll for the next value. `Ready(None)` once the sender is gone and the
    /// queue is drained.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut inner = self.inner.borrow_mut();
        if let Some(v) = inner.buf.pop_front() {
            inner.wake_sender();
            return Poll::Ready(Some(v));
        }
        if !inner.sender_alive {
            return Poll::Ready(None);
        }
        inner.recv_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    /// Wait for the next value. `None` once the sender is gone and the queue
    /// is drained.
    pub async fn recv(&self) -> Option<T> {
        poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// Values currently queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.borrow().buf.len()
    }

    /// `true` when nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Drop for LocalSender<T> {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.sender_alive = false;
        inner.wake_receiver();
    }
}

impl<T> Drop for LocalReceiver<T> {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.receiver_alive = false;
        inner.buf.clear();
        inner.wake_sender();
    }
}

impl<T> fmt::Debug for LocalSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalSender")
            .field("len", &self.len())
            .finish()
    }
}

impl<T> fmt::Debug for LocalReceiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalReceiver")
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting() -> (Arc<CountingWaker>, Waker) {
        let w = Arc::new(CountingWaker(AtomicUsize::new(0)));
        (Arc::clone(&w), Waker::from(w))
    }

    #[test]
    fn bounded_queue_rejects_at_hwm_and_accepts_after_recv() {
        let (tx, rx) = local_queue::<u32>(2);
        tx.try_send(1).unwrap();
        tx.try_send(2).unwrap();
        assert_eq!(tx.try_send(3), Err(TrySendError::Full(3)));
        assert_eq!(rx.try_recv(), Ok(Some(1)));
        tx.try_send(3).unwrap();
        assert_eq!(rx.try_recv(), Ok(Some(2)));
        assert_eq!(rx.try_recv(), Ok(Some(3)));
        assert_eq!(rx.try_recv(), Ok(None));
    }

    #[test]
    fn zero_capacity_is_unbounded() {
        let (tx, rx) = local_queue::<u32>(0);
        for i in 0..10_000 {
            tx.try_send(i).unwrap();
        }
        assert_eq!(rx.len(), 10_000);
    }

    #[test]
    fn conflating_queue_keeps_only_the_newest() {
        let (tx, rx) = conflating_queue::<u32>();
        tx.try_send(1).unwrap();
        tx.try_send(2).unwrap();
        tx.try_send(3).unwrap();
        assert_eq!(rx.try_recv(), Ok(Some(3)));
        assert_eq!(rx.try_recv(), Ok(None));
    }

    #[test]
    fn blocked_sender_is_woken_by_recv() {
        let (tx, rx) = local_queue::<u32>(1);
        tx.try_send(1).unwrap();
        let (count, waker) = counting();
        let mut cx = Context::from_waker(&waker);
        assert_eq!(tx.poll_ready(&mut cx), Poll::Pending);
        assert_eq!(rx.try_recv(), Ok(Some(1)));
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert_eq!(tx.poll_ready(&mut cx), Poll::Ready(true));
    }

    #[test]
    fn waiting_receiver_is_woken_by_send_and_by_close() {
        let (tx, rx) = local_queue::<u32>(4);
        let (count, waker) = counting();
        let mut cx = Context::from_waker(&waker);
        assert_eq!(rx.poll_recv(&mut cx), Poll::Pending);
        tx.try_send(7).unwrap();
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert_eq!(rx.poll_recv(&mut cx), Poll::Ready(Some(7)));
        assert_eq!(rx.poll_recv(&mut cx), Poll::Pending);
        drop(tx);
        assert_eq!(count.0.load(Ordering::SeqCst), 2);
        assert_eq!(rx.poll_recv(&mut cx), Poll::Ready(None));
        assert_eq!(rx.try_recv(), Err(()));
    }

    #[test]
    fn dropping_receiver_closes_the_sender() {
        let (tx, rx) = local_queue::<u32>(1);
        tx.try_send(1).unwrap();
        let (_, waker) = counting();
        let mut cx = Context::from_waker(&waker);
        assert_eq!(tx.poll_ready(&mut cx), Poll::Pending);
        drop(rx);
        assert_eq!(tx.poll_ready(&mut cx), Poll::Ready(false));
        assert_eq!(tx.try_send(2), Err(TrySendError::Closed(2)));
        assert!(!tx.is_connected());
    }
}
