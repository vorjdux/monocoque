//! The ZeroMQ sockets, built on the socket multiplexer.
//!
//! [`Socket<K>`] is one socket of kind `K`. Everything every kind shares lives
//! here: binding and connecting (any number of endpoints, any mix of TCP, IPC
//! and inproc), options, monitoring, timeouts, and the borrowed receive slot.
//! Each kind (see [`kinds`]) adds the send and receive rules of its pattern.
//!
//! A socket is `!Send` and runs on the thread that created it. Its I/O happens
//! inline in the calls that need it, and a per-socket driver task does the
//! background work while no call is in progress.

pub mod kinds;

use crate::session::SocketType;
use crate::wire::{Zmtp, ZmtpConfig};
use bytes::Bytes;
use monocoque_core::error::{MonocoqueError, Result};
use monocoque_core::monitor::{SocketMonitor, create_monitor};
use monocoque_core::msg::{Msg, MsgRef};
use monocoque_core::mux::{Hooks, PeerId, PeerInfo, SendTo, Shared, SocketCore, op};
use monocoque_core::options::SocketOptions;
use monocoque_core::transport::Stream;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

/// A ZeroMQ socket type.
pub trait Kind: Sized + 'static {
    /// The ZMTP socket type.
    const TYPE: SocketType;
    /// Pattern state the multiplexer calls into.
    type Hooks: Hooks;
    /// Per-socket state kept on the user side (REQ/REP envelopes).
    type Ext: Default;

    /// Build the pattern state for a socket with `options`.
    fn hooks(options: &SocketOptions) -> Self::Hooks;

    /// Finish setting up a new socket (SUB applies `options.subscriptions`).
    fn init(_socket: &mut Socket<Self>) {}
}

/// One ZeroMQ socket. See the [module docs](self).
pub struct Socket<K: Kind> {
    pub(crate) core: Shared<Zmtp, K::Hooks>,
    slot: Msg,
    slot_peer: Option<PeerId>,
    pub(crate) ext: K::Ext,
}

impl<K: Kind> Default for Socket<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Kind> Socket<K> {
    /// A socket with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(SocketOptions::default())
    }

    /// A socket with `options`.
    #[must_use]
    pub fn with_options(options: SocketOptions) -> Self {
        let hooks = K::hooks(&options);
        let core = SocketCore::new(
            ZmtpConfig {
                socket_type: K::TYPE,
            },
            options,
            hooks,
        );
        let mut socket = Self {
            core,
            slot: Msg::new(),
            slot_peer: None,
            ext: K::Ext::default(),
        };
        K::init(&mut socket);
        socket
    }

    /// Bind `endpoint` (`tcp://`, `ipc://` or `inproc://`) and accept peers on
    /// it. Returns the endpoint actually bound, with a TCP port of 0 resolved.
    pub async fn bind(&mut self, endpoint: &str) -> Result<String> {
        SocketCore::bind(&self.core, endpoint)
            .await
            .map(|e| e.to_string())
    }

    /// Start connecting to `endpoint`, reconnecting whenever the connection
    /// drops. Returns once dialing has begun; messages wait for the peer.
    pub async fn connect(&mut self, endpoint: &str) -> Result<()> {
        SocketCore::connect(&self.core, endpoint)
    }

    /// Stop dialing `endpoint` and drop its connection.
    pub fn disconnect(&mut self, endpoint: &str) -> Result<()> {
        self.core.borrow_mut().disconnect(endpoint)
    }

    /// Stop accepting on `endpoint`; peers it already accepted stay.
    pub fn unbind(&mut self, endpoint: &str) -> Result<()> {
        self.core.borrow_mut().unbind(endpoint)
    }

    /// Handshake an already-connected stream and add it as a peer.
    pub async fn attach(&mut self, stream: impl Into<Stream>) -> Result<PeerId> {
        SocketCore::attach(&self.core, stream.into()).await
    }

    /// The socket's options.
    #[must_use]
    pub fn options(&self) -> SocketOptions {
        self.core.borrow().options().clone()
    }

    /// Replace the socket's options. HWMs apply to existing peers at once;
    /// connection settings apply to connections made afterwards.
    pub fn set_options(&mut self, options: SocketOptions) {
        self.core.borrow_mut().set_options(options);
    }

    /// Change one option.
    pub fn update_options(&mut self, f: impl FnOnce(SocketOptions) -> SocketOptions) {
        let next = f(self.options());
        self.set_options(next);
    }

    /// Receive lifecycle events (connected, accepted, disconnected, ...).
    pub fn monitor(&mut self) -> SocketMonitor {
        let (tx, rx) = create_monitor();
        self.core.borrow_mut().set_monitor(tx);
        rx
    }

    /// This socket's type.
    #[must_use]
    pub const fn socket_type(&self) -> SocketType {
        K::TYPE
    }

    /// Number of connected peers.
    #[must_use]
    pub fn peer_count(&self) -> usize {
        self.core.borrow().peer_count()
    }

    /// The most recently bound endpoint (`ZMQ_LAST_ENDPOINT`).
    #[must_use]
    pub fn last_endpoint(&self) -> Option<String> {
        self.core
            .borrow()
            .bound_endpoints()
            .last()
            .map(ToString::to_string)
    }

    /// A metadata property of the peer the last received message came from
    /// (`zmq_msg_gets`): `Socket-Type`, `Identity`, `Peer-Address`, and any
    /// property the handshake carried.
    #[must_use]
    pub fn peer_property(&self, name: &str) -> Option<Bytes> {
        let id = self.slot_peer?;
        self.core.borrow().peer_info(id)?.property(name).cloned()
    }

    /// Everything known about the peer the last received message came from.
    #[must_use]
    pub fn last_peer(&self) -> Option<PeerInfo> {
        let id = self.slot_peer?;
        self.core.borrow().peer_info(id).cloned()
    }

    /// Release writes held by `write_coalescing` and wait until everything
    /// queued has been written.
    pub async fn flush(&mut self) -> Result<()> {
        with_timeout(
            op(&self.core, SocketCore::poll_flushed),
            self.send_timeout(),
        )
        .await
    }

    /// Close the socket, waiting up to `linger` for queued messages to be
    /// written. Dropping a socket closes it the same way without waiting.
    pub async fn close(self) -> Result<()> {
        let linger = self.options().linger;
        let drained = op(&self.core, SocketCore::poll_flushed);
        match linger {
            Some(d) if d.is_zero() => {}
            Some(d) => {
                let _ = with_timeout(drained, Some(d)).await;
            }
            None => drained.await,
        }
        Ok(())
    }

    /// The wake routing a multi-socket poll parks on.
    #[must_use]
    pub fn wakeups(&self) -> Arc<monocoque_core::mux::Wakeups> {
        Arc::clone(self.core.borrow().wakeups())
    }

    /// `true` if a receive would return a message without waiting.
    #[must_use]
    pub fn has_message(&self) -> bool {
        self.core.borrow_mut().has_ready()
    }

    /// `true` if a send would be accepted without waiting.
    #[must_use]
    pub fn can_send(&self) -> bool {
        self.core.borrow_mut().any_writable()
    }

    // ── shared building blocks for the kinds ─────────────────────────────

    fn recv_timeout(&self) -> Option<Duration> {
        self.core.borrow().options().recv_timeout
    }

    fn send_timeout(&self) -> Option<Duration> {
        self.core.borrow().options().send_timeout
    }

    /// Next message from any peer, fair-queued, honoring `recv_timeout`.
    pub(crate) async fn next_into_slot(&mut self) -> Result<PeerId> {
        let timeout = self.recv_timeout();
        let slot = &mut self.slot;
        let peer = with_timeout(
            op(&self.core, move |c, cx| c.poll_recv_into(cx, slot)),
            timeout,
        )
        .await?;
        self.slot_peer = Some(peer);
        Ok(peer)
    }

    /// The receive slot, filled by `next_into_slot`.
    pub(crate) const fn slot_mut(&mut self) -> &mut Msg {
        &mut self.slot
    }

    /// Lend out the receive slot.
    pub(crate) fn lend(&mut self) -> MsgRef<'_> {
        MsgRef::new(&mut self.slot)
    }

    /// Next message from `peer` only, into the receive slot. `Ok(false)` once
    /// that peer is gone.
    pub(crate) async fn next_from_into_slot(&mut self, peer: PeerId) -> Result<bool> {
        let timeout = self.recv_timeout();
        let slot = &mut self.slot;
        let got = with_timeout(
            op(&self.core, move |c, cx| {
                c.poll_recv_from_into(cx, peer, slot)
            }),
            timeout,
        )
        .await?;
        if got {
            self.slot_peer = Some(peer);
        }
        Ok(got)
    }

    /// Send to the next peer with room, waiting while none has.
    pub(crate) async fn send_lb(&mut self, msg: Msg) -> Result<PeerId> {
        self.send_lb_prefixed(&[], msg).await
    }

    /// Like `send_lb`, with envelope frames ahead of the message.
    pub(crate) async fn send_lb_prefixed(&mut self, prefix: &[Bytes], msg: Msg) -> Result<PeerId> {
        let timeout = self.send_timeout();
        let mut slot = Some(msg);
        with_timeout(
            op(&self.core, move |c, cx| {
                c.poll_send_lb_prefixed(cx, prefix, &mut slot)
            }),
            timeout,
        )
        .await?
    }

    /// Send to `peer` with envelope frames ahead of the message, waiting
    /// while it is at its HWM.
    pub(crate) async fn send_to_waiting_prefixed(
        &mut self,
        peer: PeerId,
        prefix: &[Bytes],
        msg: Msg,
    ) -> Result<()> {
        let timeout = self.send_timeout();
        let mut slot = Some(msg);
        with_timeout(
            op(&self.core, move |c, cx| {
                c.poll_send_to_prefixed(cx, peer, prefix, &mut slot)
            }),
            timeout,
        )
        .await?
    }

    /// Queue to `peer` without waiting.
    pub(crate) fn send_to_now(&mut self, peer: PeerId, msg: Msg) -> SendTo {
        self.core.borrow_mut().send_to(peer, msg)
    }
}

impl<K: Kind> Drop for Socket<K> {
    fn drop(&mut self) {
        if let Ok(mut core) = self.core.try_borrow_mut() {
            core.close();
        }
    }
}

impl<K: Kind> std::fmt::Debug for Socket<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socket")
            .field("type", &K::TYPE)
            .field("peers", &self.peer_count())
            .finish_non_exhaustive()
    }
}

/// Run `fut` under `timeout`: `None` waits forever, zero polls once and fails
/// with [`MonocoqueError::Again`], otherwise fails with
/// [`MonocoqueError::TimedOut`] when it elapses. Nothing in flight is
/// cancelled by a timeout; only the wait ends.
pub(crate) async fn with_timeout<T>(
    fut: impl Future<Output = T>,
    timeout: Option<Duration>,
) -> Result<T> {
    match timeout {
        None => Ok(fut.await),
        Some(d) if d.is_zero() => {
            let mut fut = pin!(fut);
            poll_fn(|cx| {
                Poll::Ready(match fut.as_mut().poll(cx) {
                    Poll::Ready(v) => Ok(v),
                    Poll::Pending => Err(MonocoqueError::Again),
                })
            })
            .await
        }
        Some(d) => monocoque_core::rt::timeout(d, fut)
            .await
            .map_err(|_| MonocoqueError::TimedOut),
    }
}
