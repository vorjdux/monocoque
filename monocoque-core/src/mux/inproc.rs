//! The inproc endpoint registry.
//!
//! An inproc connection moves `Msg` values between two sockets with no codec
//! and no copy. Two sockets on the same thread are joined by a pair of
//! [`LocalQueue`](crate::localq)s, which use no atomics; sockets on different
//! threads by a pair of `flume` channels. The choice is made at connect time by
//! comparing the connecting thread with the binder's.
//!
//! A binder registers its name twice: in a thread-local table holding a
//! same-thread request queue, and in the process-wide table holding a channel
//! that crosses threads. A connect checks its own thread's table first.

use super::PeerInfo;
use crate::localq::{LocalReceiver, LocalSender, local_queue};
use crate::msg::Msg;
use futures::task::AtomicWaker;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Context;
use std::thread::ThreadId;

/// One side of an inproc connection.
pub enum Link {
    /// Both sockets on one thread.
    Local {
        /// Messages toward the other side.
        tx: LocalSender<Msg>,
        /// Messages from the other side.
        rx: LocalReceiver<Msg>,
    },
    /// The sockets are on different threads.
    Remote {
        /// Messages toward the other side.
        tx: RemoteTx,
        /// Messages from the other side.
        rx: RemoteRx,
    },
}

/// Raised by a cross-thread receiver when it takes a message, for a sender
/// that found the channel at its HWM. flume itself only wakes senders blocked
/// inside its own send futures, and a socket checks for room without one.
#[derive(Default)]
struct Room {
    waiting: AtomicBool,
    waker: AtomicWaker,
}

/// The sending end of one direction of a cross-thread link.
pub struct RemoteTx {
    tx: flume::Sender<Msg>,
    room: Arc<Room>,
}

/// The receiving end of one direction of a cross-thread link.
pub struct RemoteRx {
    rx: flume::Receiver<Msg>,
    room: Arc<Room>,
}

impl RemoteTx {
    /// Queue `msg` without waiting. `false` if the channel is full or the
    /// receiver is gone; the message is dropped then.
    #[must_use]
    pub fn try_send(&self, msg: Msg) -> bool {
        self.tx.try_send(msg).is_ok()
    }

    /// Messages queued and not yet taken.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tx.len()
    }

    /// `true` when nothing is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tx.is_empty()
    }

    /// `true` once the receiver is gone.
    #[must_use]
    pub fn is_disconnected(&self) -> bool {
        self.tx.is_disconnected()
    }

    /// Have the receiver wake `cx` when it next takes a message. Wakes it
    /// right away if room appeared while registering, so no wakeup is lost.
    pub fn watch_room(&self, cx: &mut Context<'_>, hwm: usize) {
        self.room.waker.register(cx.waker());
        self.room.waiting.store(true, Ordering::SeqCst);
        if self.tx.is_disconnected() || self.tx.len() < hwm {
            cx.waker().wake_by_ref();
        }
    }
}

impl RemoteRx {
    /// The next message, or `None` once the sender is gone.
    pub async fn recv(&self) -> Option<Msg> {
        let msg = self.rx.recv_async().await.ok();
        if self.room.waiting.swap(false, Ordering::SeqCst) {
            self.room.waker.wake();
        }
        msg
    }
}

/// A connection offered to a binder: the connector's identity and the
/// binder's end of the link.
pub struct Offer {
    /// Who is connecting.
    pub info: PeerInfo,
    /// The binder's end.
    pub link: Link,
}

/// A cross-thread offer; everything in it is `Send`.
pub struct RemoteOffer {
    /// Who is connecting.
    pub info: PeerInfo,
    /// Messages toward the connector.
    pub tx: RemoteTx,
    /// Messages from the connector.
    pub rx: RemoteRx,
}

struct GlobalBinding {
    thread: ThreadId,
    info: PeerInfo,
    offers: flume::Sender<RemoteOffer>,
}

struct LocalBinding {
    info: PeerInfo,
    offers: LocalSender<Offer>,
}

fn global() -> &'static Mutex<HashMap<String, GlobalBinding>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, GlobalBinding>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

thread_local! {
    static LOCAL: RefCell<HashMap<String, LocalBinding>> = RefCell::default();
}

/// The binder's receiving ends for connection offers.
pub struct Binding {
    name: String,
    /// Offers from sockets on this thread.
    pub local: LocalReceiver<Offer>,
    /// Offers from sockets on other threads.
    pub remote: flume::Receiver<RemoteOffer>,
}

impl Drop for Binding {
    fn drop(&mut self) {
        // A runtime that keeps its executor in a thread-local (smol) drops its
        // tasks during thread teardown, possibly after this table is gone.
        let _ = LOCAL.try_with(|l| l.borrow_mut().remove(&self.name));
        let mut g = global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.get(&self.name)
            .is_some_and(|b| b.thread == std::thread::current().id())
        {
            g.remove(&self.name);
        }
    }
}

/// Register `name`, advertising `info` to connectors.
pub fn bind(name: &str, info: PeerInfo) -> io::Result<Binding> {
    let mut g = global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if g.contains_key(name) {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("inproc://{name} is already bound"),
        ));
    }
    let (remote_tx, remote_rx) = flume::unbounded();
    let (local_tx, local_rx) = local_queue(0);
    g.insert(
        name.to_string(),
        GlobalBinding {
            thread: std::thread::current().id(),
            info: info.clone(),
            offers: remote_tx,
        },
    );
    drop(g);
    LOCAL.with(|l| {
        l.borrow_mut().insert(
            name.to_string(),
            LocalBinding {
                info,
                offers: local_tx,
            },
        );
    });
    Ok(Binding {
        name: name.to_string(),
        local: local_rx,
        remote: remote_rx,
    })
}

/// Connect to `name` as `info`. Returns this side of the link and the
/// binder's identity. `hwm` bounds each direction (`0` for unbounded).
pub fn connect(name: &str, info: PeerInfo, hwm: usize) -> io::Result<(Link, PeerInfo)> {
    let local = LOCAL.with(|l| {
        l.borrow()
            .get(name)
            .map(|b| (b.info.clone(), b.offers.is_connected()))
    });
    if let Some((binder, true)) = local {
        let (to_binder, binder_rx) = local_queue(hwm);
        let (binder_tx, from_binder) = local_queue(hwm);
        let sent = LOCAL.with(|l| {
            l.borrow().get(name).map(|b| {
                b.offers
                    .try_send(Offer {
                        info,
                        link: Link::Local {
                            tx: binder_tx,
                            rx: binder_rx,
                        },
                    })
                    .is_ok()
            })
        });
        if sent == Some(true) {
            return Ok((
                Link::Local {
                    tx: to_binder,
                    rx: from_binder,
                },
                binder,
            ));
        }
        return Err(refused(name));
    }

    let found = global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(name)
        .map(|b| (b.offers.clone(), b.info.clone()));
    let Some((offers, binder)) = found else {
        return Err(refused(name));
    };
    let (to_binder, binder_rx) = bounded(hwm);
    let (binder_tx, from_binder) = bounded(hwm);
    offers
        .send(RemoteOffer {
            info,
            tx: binder_tx,
            rx: binder_rx,
        })
        .map_err(|_| refused(name))?;
    Ok((
        Link::Remote {
            tx: to_binder,
            rx: from_binder,
        },
        binder,
    ))
}

fn bounded(hwm: usize) -> (RemoteTx, RemoteRx) {
    let (tx, rx) = if hwm == 0 {
        flume::unbounded()
    } else {
        flume::bounded(hwm)
    };
    let room = Arc::new(Room::default());
    (
        RemoteTx {
            tx,
            room: Arc::clone(&room),
        },
        RemoteRx { rx, room },
    )
}

fn refused(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!("inproc://{name} is not bound"),
    )
}

/// Names currently bound in this process.
pub fn bound_names() -> Vec<String> {
    global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .cloned()
        .collect()
}
