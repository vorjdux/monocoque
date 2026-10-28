//! Background work a socket does on its own: accepting, connecting and
//! reconnecting, heartbeats, lingering on close.
//!
//! One driver task per socket polls the socket's children whenever the user
//! task is not doing so. Its contract:
//!
//! - it services only work that options enabled (accepting on bound endpoints,
//!   dialing connected ones, heartbeats when `heartbeat_ivl` is set, linger on
//!   close);
//! - it never reorders, delays or alters user messages; it only moves them
//!   between the wire and the per-peer queues as the user's own calls would;
//! - it is strictly event driven: when idle and no heartbeat is configured it
//!   takes zero wakeups;
//! - it runs on the socket's thread, so a thread blocked outside the runtime
//!   also stalls the socket's background progress.

use super::{Event, EventQueue, Hooks, Protocol, Shared};
use crate::endpoint::Endpoint;
use crate::options::SocketOptions;
use crate::reconnect::ReconnectState;
use crate::transport::{Listener, Stream};
use std::cell::{Cell, RefCell};
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// The task that drives a socket while its user is elsewhere.
pub struct Driver<P: Protocol, H: Hooks> {
    pub(super) core: Shared<P, H>,
}

impl<P: Protocol, H: Hooks> Future for Driver<P, H> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut core = self.core.borrow_mut();
        core.wakeups.set_driver(cx.waker());
        core.progress();
        if core.finished() {
            core.shutdown_now();
            core.wakeups.clear_driver();
            core.driver_running = false;
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

/// A one-shot, re-armable notification between a connector and the core.
#[derive(Default)]
pub struct Signal {
    fired: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

impl Signal {
    /// Fire the signal, waking its waiter.
    pub fn fire(&self) {
        self.fired.set(true);
        if let Some(w) = self.waker.borrow_mut().take() {
            w.wake();
        }
    }

    /// Wait until fired, then re-arm.
    pub async fn wait(&self) {
        poll_fn(|cx| {
            if self.fired.replace(false) {
                Poll::Ready(())
            } else {
                *self.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
}

/// Spread a backoff delay uniformly across `[delay/2, delay]`, so a fleet of
/// clients that lost the same server does not redial it in lockstep.
fn jittered(delay: Duration) -> Duration {
    use rand::RngExt;
    if delay.is_zero() {
        return delay;
    }
    let half = u64::try_from(delay.as_nanos() / 2).unwrap_or(u64::MAX);
    Duration::from_nanos(half + rand::rng().random_range(0..=half))
}

/// Dial `endpoint` until it connects, hand the stream to the core, wait for
/// that connection to end, and dial again after a backoff.
pub async fn connect_loop(
    index: usize,
    endpoint: Endpoint,
    events: Rc<EventQueue>,
    down: Rc<Signal>,
    options: Rc<SocketOptions>,
) {
    let mut backoff = ReconnectState::new(&options);
    loop {
        match Stream::connect(&endpoint).await {
            Ok(stream) => {
                if stream.configure(&options).is_ok() {
                    events.push(Event::Connected {
                        connector: index,
                        stream,
                    });
                    // The core fires this when the handshake fails or the
                    // resulting peer goes away; `true` in `up` means the
                    // connection had been established.
                    down.wait().await;
                    backoff.reset();
                }
            }
            Err(e) => {
                events.push(Event::ConnectFailed {
                    connector: index,
                    reason: e.to_string(),
                });
            }
        }
        crate::rt::sleep(jittered(backoff.next_delay())).await;
    }
}

/// Accept connections on `listener` and hand each to the core.
pub async fn accept_loop(
    index: usize,
    listener: Listener,
    events: Rc<EventQueue>,
    options: Rc<SocketOptions>,
) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                if stream.configure(&options).is_ok() {
                    events.push(Event::Accepted {
                        listener: index,
                        stream,
                    });
                }
            }
            Err(e) => {
                // Out of descriptors: back off instead of spinning on a
                // listener that will keep failing until something closes.
                if crate::transport::is_fd_exhaustion(&e) {
                    crate::rt::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

/// Accept inproc offers for a bound name and hand each to the core.
pub async fn inproc_accept_loop(
    index: usize,
    binding: super::inproc::Binding,
    events: Rc<EventQueue>,
) {
    use futures::future::{Either, select};
    let binding = Rc::new(binding);
    loop {
        let local = binding.local.recv();
        let remote = binding.remote.recv_async();
        let offer = match select(std::pin::pin!(local), std::pin::pin!(remote)).await {
            Either::Left((Some(offer), _)) => offer,
            Either::Right((Ok(r), _)) => super::inproc::Offer {
                info: r.info,
                link: super::inproc::Link::Remote { tx: r.tx, rx: r.rx },
            },
            Either::Left((None, _)) | Either::Right((Err(_), _)) => return,
        };
        events.push(Event::InprocOffer {
            listener: index,
            offer,
        });
    }
}

/// Raise a heartbeat tick every `ivl`.
pub async fn heartbeat_loop(ivl: Duration, events: Rc<EventQueue>) {
    loop {
        crate::rt::sleep(ivl).await;
        events.push(Event::Tick);
    }
}

/// Fire once after `linger`, ending a close that is still draining.
pub async fn linger_timer(linger: Duration, events: Rc<EventQueue>) {
    crate::rt::sleep(linger).await;
    events.push(Event::LingerExpired);
}
