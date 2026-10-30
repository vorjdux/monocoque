//! `SocketCore`: the multiplexer every socket is built on.
//!
//! A socket is a set of peers. Each wire peer runs one persistent reader loop
//! and one persistent writer loop (see [`peer`]); each inproc peer moves `Msg`
//! values through a queue with no codec. The core keeps the peer table, a fair
//! queue of peers with messages ready, and a round-robin cursor for load
//! balancing, and it routes nothing itself: the socket patterns built on it
//! decide which peer a message goes to.
//!
//! There is no channel between the wire and the user. A complete message waits
//! in its peer's ready queue until a receive takes it; while the user waits,
//! the user's own poll runs the reader loop that produces it (see
//! [`children`]). Background work, including accepting, dialing, heartbeats
//! and linger, runs on one driver task per socket only when the user is not
//! polling.
//!
//! The core never knows the wire protocol. A [`Protocol`] supplies the
//! handshake and a per-connection [`Codec`]; the ZMTP crate implements it.
//!
//! Rule for everything that touches a `Shared` core: borrow, do the work, drop
//! the borrow; never hold it across an `.await`. Every operation here is a
//! poll function, so the rule holds by construction.

pub mod children;
mod driver;
pub mod inproc;
pub mod out;
pub mod peer;
#[cfg(test)]
mod tests;

pub use children::{ChildId, Children, Wakeups};
pub use driver::Signal;
pub use out::{OutQueue, WriteJob};

use crate::endpoint::Endpoint;
use crate::error::{MonocoqueError, Result};
use crate::monitor::{SocketEvent, SocketEventSender};
use crate::msg::{Metadata, Msg};
use crate::options::SocketOptions;
use crate::transport::{Listener, Stream};
use bytes::{Bytes, BytesMut};
use peer::{Outbound, PeerState};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// The byte threshold below which a wire peer holds writes, if coalescing.
fn coalesce_threshold(options: &SocketOptions) -> Option<usize> {
    options
        .write_coalescing
        .then_some(options.write_coalesce_threshold)
}

/// Per-connection framing state of a protocol.
pub trait Codec: 'static {
    /// Decode every complete message at the front of `buf`, handing each to
    /// `deliver` and consuming its bytes. A partial frame is left in place; the
    /// decoder may `reserve` room for the rest of it. Protocol replies (a PONG)
    /// go into `replies`.
    fn decode(
        &mut self,
        buf: &mut BytesMut,
        deliver: &mut dyn FnMut(Msg),
        replies: &mut OutQueue,
    ) -> Result<()>;

    /// `true` while `decode` has swapped in a buffer sized to one frame, which
    /// reads must fill but not grow.
    fn filling_frame(&self) -> bool {
        false
    }

    /// Encode `msg` into `out`.
    fn encode(&mut self, msg: &Msg, out: &mut OutQueue) -> Result<()>;

    /// Encode `prefix` frames followed by `msg`, as one message. Envelopes
    /// (REQ's delimiter, REP's routing stack) go through here so the user's
    /// message is never rebuilt to carry them.
    fn encode_prefixed(&mut self, prefix: &[Bytes], msg: &Msg, out: &mut OutQueue) -> Result<()> {
        let mut whole: Msg = prefix.iter().cloned().collect();
        whole.extend((0..msg.len()).filter_map(|i| msg.frame_bytes(i)));
        self.encode(&whole, out)
    }

    /// `true` when this connection's encoding of a message does not depend on
    /// per-connection state, so a fan-out may encode once and share the bytes.
    fn shares_encoding(&self) -> bool {
        false
    }

    /// Queue a heartbeat probe advertising `ttl`.
    fn encode_ping(&mut self, _ttl: Duration, _out: &mut OutQueue) {}

    /// The time-to-live the peer last advertised, if any.
    fn peer_ttl(&self) -> Option<Duration> {
        None
    }

    /// A message to receive from the peer before anything it sends, standing
    /// in for something its protocol version does not say (a ZMTP 1.0
    /// subscriber's implicit subscription to everything).
    fn initial_message(&mut self) -> Option<Msg> {
        None
    }
}

/// A wire protocol: how to handshake and how to frame.
pub trait Protocol: 'static {
    /// Per-socket protocol configuration.
    type Config: 'static;
    /// Per-connection framing state.
    type Codec: Codec;

    /// Handshake a freshly connected stream. `outgoing` is true on the
    /// connecting side.
    fn handshake(
        cfg: Rc<Self::Config>,
        options: Rc<SocketOptions>,
        stream: Rc<Stream>,
        outgoing: bool,
    ) -> impl Future<Output = Result<(Self::Codec, PeerInfo)>> + 'static;

    /// This socket's type name, as an inproc peer sees it.
    fn socket_type(cfg: &Self::Config) -> &'static str;

    /// May an inproc peer of type `peer` connect to this socket?
    fn compatible(cfg: &Self::Config, peer: &str) -> bool;

    /// Encode `msg` once into `buf` for sending to every peer whose codec
    /// [`shares_encoding`](Codec::shares_encoding).
    fn encode_shared(cfg: &Self::Config, msg: &Msg, buf: &mut BytesMut);
}

/// Pattern hooks: what a socket type does when peers come and go.
pub trait Hooks: 'static {
    /// A peer finished its handshake. Return `false` to refuse it.
    fn on_peer_up(&mut self, _id: PeerId, _info: &PeerInfo) -> bool {
        true
    }

    /// A peer went away. Messages it sent before closing may still be
    /// waiting to be received.
    fn on_peer_down(&mut self, _id: PeerId, _info: &PeerInfo) {}

    /// The last message from a peer that went away has been received (or it
    /// left none), so `id` will not be seen again.
    fn on_peer_freed(&mut self, _id: PeerId) {}

    /// Messages every new peer gets first, built when it comes up (SUB's
    /// current subscriptions).
    fn greeting(&self) -> Vec<Msg> {
        Vec::new()
    }

    /// When `true`, inbound messages are handed to [`on_inbound`] as soon as
    /// they arrive, by whichever task is polling, instead of waiting for a
    /// receive. PUB uses this to apply subscriptions it never "receives".
    ///
    /// [`on_inbound`]: Self::on_inbound
    fn eager_inbound(&self) -> bool {
        false
    }

    /// Inspect an inbound message. Return it to make it receivable, or `None`
    /// to consume it.
    fn on_inbound(&mut self, _id: PeerId, msg: Msg) -> Option<Msg> {
        Some(msg)
    }
}

impl Hooks for () {}

/// Identifies one peer of one socket. Stale ids never alias a later peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId {
    index: u32,
    generation: u32,
}

impl PeerId {
    /// A small integer unique among this socket's live peers.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// A value unique among every peer this socket has ever had.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }

    /// The id [`as_u64`](Self::as_u64) was taken from.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // each half is exactly 32 bits
    pub const fn from_u64(v: u64) -> Self {
        Self {
            index: (v & 0xFFFF_FFFF) as u32,
            generation: (v >> 32) as u32,
        }
    }
}

/// What a peer told us about itself in its handshake.
#[derive(Clone, Debug, Default)]
pub struct PeerInfo {
    /// The peer's socket type name.
    pub socket_type: &'static str,
    /// The routing identity the peer announced, if any.
    pub identity: Option<Bytes>,
    /// The peer's address as ZAP reports it: its IP for TCP, empty otherwise.
    pub address: String,
    /// The endpoint the connection was made through, when known.
    pub endpoint: Option<Endpoint>,
    /// Connection metadata (handshake properties, the ZAP user id), built once
    /// per connection and shared with every message received from it.
    pub metadata: Arc<Metadata>,
}

impl PeerInfo {
    /// Look up one metadata property by name (ASCII case-insensitive).
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&Bytes> {
        self.metadata.get(name)
    }

    /// Describe a peer on this host (inproc): its socket type, its identity
    /// and the custom properties it announces.
    #[must_use]
    pub fn local(socket_type: &'static str, options: &SocketOptions) -> Self {
        let identity = options.routing_id.clone();
        let mut meta = Metadata::new();
        meta.insert("Socket-Type", Bytes::from_static(socket_type.as_bytes()));
        if let Some(id) = identity.as_ref().filter(|id| !id.is_empty()) {
            meta.insert("Identity", id.clone());
        }
        for (k, v) in &options.metadata {
            meta.insert(k.clone(), v.clone());
        }
        Self {
            socket_type,
            identity,
            metadata: Arc::new(meta),
            ..Self::default()
        }
    }
}

/// Why a peer went away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The peer closed the connection.
    Eof,
    /// A read or write failed.
    Io(io::ErrorKind),
    /// The peer broke the protocol.
    Protocol(String),
    /// No traffic within the heartbeat timeout.
    HeartbeatTimeout,
    /// The local side dropped it.
    Local,
}

/// Where a connection came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Listener(usize),
    Connector(usize),
    Attached,
}

/// Something a child future reports to the core.
pub enum Event {
    /// A listener accepted a stream; it needs a handshake.
    Accepted {
        /// Which listener.
        listener: usize,
        /// The accepted stream.
        stream: Stream,
    },
    /// A connector dialed a stream; it needs a handshake.
    Connected {
        /// Which connector.
        connector: usize,
        /// The dialed stream.
        stream: Stream,
    },
    /// A connector failed to dial.
    ConnectFailed {
        /// Which connector.
        connector: usize,
        /// Why.
        reason: String,
    },
    /// An inproc connector offered a link.
    InprocOffer {
        /// Which binding.
        listener: usize,
        /// The connector's identity and link.
        offer: inproc::Offer,
    },
    /// A peer has messages ready.
    Readable(PeerId),
    /// A peer finished a write, so it may have send room again.
    Writable(PeerId),
    /// A peer is gone.
    Closed(PeerId, CloseReason),
    /// Heartbeat interval elapsed.
    Tick,
    /// The close linger period ended.
    LingerExpired,
}

/// The queue children report events through.
#[derive(Default)]
pub struct EventQueue {
    q: RefCell<VecDeque<Event>>,
}

impl EventQueue {
    /// Report an event.
    pub fn push(&self, ev: Event) {
        self.q.borrow_mut().push_back(ev);
    }

    fn pop(&self) -> Option<Event> {
        self.q.borrow_mut().pop_front()
    }
}

struct Handshaken<P: Protocol> {
    origin: Origin,
    stream: Rc<Stream>,
    result: Result<(P::Codec, PeerInfo)>,
}

struct PeerEntry<C> {
    state: Rc<RefCell<PeerState<C>>>,
    info: PeerInfo,
    children: [Option<ChildId>; 2],
    origin: Origin,
    in_fair: bool,
    closed: bool,
}

struct Connector {
    endpoint: Endpoint,
    child: ChildId,
    down: Rc<Signal>,
    peer: Option<PeerId>,
    /// `connect_routing_id`: the identity this connection's peers are routed
    /// by, in place of the one they announce.
    routing_id: Option<Bytes>,
}

struct Bound {
    endpoint: Endpoint,
    child: ChildId,
}

/// Size of the region fan-out encodings are carved from.
const SHARED_WIRE_REGION: usize = 64 * 1024;

/// A socket core shared between the user's handle and its driver task.
pub type Shared<P, H> = Rc<RefCell<SocketCore<P, H>>>;

/// What [`SocketCore::send_to`] did with a message.
#[derive(Debug)]
pub enum SendTo {
    /// Queued toward the peer.
    Sent,
    /// The peer is at its send high-water mark; the message is handed back.
    Full(Msg),
    /// No such live peer; the message is handed back.
    Gone(Msg),
}

/// The multiplexer behind one socket. See the [module docs](self).
pub struct SocketCore<P: Protocol, H: Hooks> {
    cfg: Rc<P::Config>,
    options: Rc<SocketOptions>,
    hooks: H,
    children: Children,
    wakeups: Arc<Wakeups>,
    events: Rc<EventQueue>,
    handshaken: Rc<RefCell<VecDeque<Handshaken<P>>>>,
    peers: Vec<Option<PeerEntry<P::Codec>>>,
    generations: Vec<u32>,
    free_peers: Vec<u32>,
    live_peers: usize,
    fair: VecDeque<PeerId>,
    inbox: VecDeque<(PeerId, Msg)>,
    lb: Vec<PeerId>,
    lb_cursor: usize,
    listeners: Vec<Option<Bound>>,
    connectors: Vec<Option<Connector>>,
    greeting: Vec<Msg>,
    /// `connect_routing_id`, until the first connect takes it.
    connect_routing_id: Option<Bytes>,
    monitor: Option<SocketEventSender>,
    heartbeat: Option<ChildId>,
    shared_wire: BytesMut,
    driver_running: bool,
    closing: bool,
    linger_expired: bool,
}

impl<P: Protocol, H: Hooks> SocketCore<P, H> {
    /// Build a core with no peers. Nothing runs until a bind, connect or
    /// attach starts the driver.
    pub fn new(cfg: P::Config, options: SocketOptions, hooks: H) -> Shared<P, H> {
        let wakeups: Arc<Wakeups> = Arc::default();
        let connect_routing_id = options.connect_routing_id.clone();
        Rc::new(RefCell::new(Self {
            cfg: Rc::new(cfg),
            options: Rc::new(options),
            hooks,
            children: Children::new(Arc::clone(&wakeups)),
            wakeups,
            events: Rc::default(),
            handshaken: Rc::default(),
            peers: Vec::new(),
            generations: Vec::new(),
            free_peers: Vec::new(),
            live_peers: 0,
            fair: VecDeque::new(),
            inbox: VecDeque::new(),
            lb: Vec::new(),
            lb_cursor: 0,
            listeners: Vec::new(),
            connectors: Vec::new(),
            greeting: Vec::new(),
            connect_routing_id,
            monitor: None,
            heartbeat: None,
            shared_wire: BytesMut::new(),
            driver_running: false,
            closing: false,
            linger_expired: false,
        }))
    }

    // ── accessors ────────────────────────────────────────────────────────

    /// The protocol configuration.
    pub fn config(&self) -> &P::Config {
        &self.cfg
    }

    /// Socket options.
    pub fn options(&self) -> &SocketOptions {
        &self.options
    }

    /// Replace socket options. Applies to connections made from now on, and
    /// to the HWMs of existing peers.
    pub fn set_options(&mut self, options: SocketOptions) {
        self.options = Rc::new(options);
        let conflate = self.conflates();
        for entry in self.peers.iter().flatten() {
            let mut st = entry.state.borrow_mut();
            st.recv_hwm = self.options.recv_hwm;
            st.send_hwm = self.options.send_hwm;
            st.conflate = conflate;
            st.set_hold(coalesce_threshold(&self.options));
        }
    }

    /// Whether peers keep only their newest inbound message. A socket that
    /// consumes inbound traffic as protocol (a publisher's subscriptions)
    /// never conflates it.
    fn conflates(&self) -> bool {
        self.options.conflate && !self.hooks.eager_inbound()
    }

    /// The socket pattern's state.
    pub const fn hooks(&self) -> &H {
        &self.hooks
    }

    /// The socket pattern's state, mutably.
    pub const fn hooks_mut(&mut self) -> &mut H {
        &mut self.hooks
    }

    /// Wake routing; a poll across sockets parks on several of these.
    pub const fn wakeups(&self) -> &Arc<Wakeups> {
        &self.wakeups
    }

    /// Route lifecycle events to `sender`.
    pub fn set_monitor(&mut self, sender: SocketEventSender) {
        self.monitor = Some(sender);
    }

    fn emit(&self, ev: SocketEvent) {
        if let Some(m) = &self.monitor {
            crate::monitor::emit(m, ev);
        }
    }

    /// Number of live peers.
    pub const fn peer_count(&self) -> usize {
        self.live_peers
    }

    /// Live peers, in connection order.
    pub fn peer_ids(&self) -> &[PeerId] {
        &self.lb
    }

    fn entry(&self, id: PeerId) -> Option<&PeerEntry<P::Codec>> {
        let i = id.index as usize;
        if self.generations.get(i) != Some(&id.generation) {
            return None;
        }
        self.peers.get(i).and_then(Option::as_ref)
    }

    /// What `id` announced in its handshake, while it is known.
    pub fn peer_info(&self, id: PeerId) -> Option<&PeerInfo> {
        self.entry(id).map(|e| &e.info)
    }

    /// `true` while `id` is connected.
    pub fn is_live(&self, id: PeerId) -> bool {
        self.entry(id).is_some_and(|e| !e.closed)
    }

    /// Messages sent to every peer as it comes up, before anything else
    /// (SUB uses this for its subscriptions).
    pub fn set_greeting(&mut self, greeting: Vec<Msg>) {
        self.greeting = greeting;
    }

    /// The bound endpoints, with OS-assigned ports resolved.
    pub fn bound_endpoints(&self) -> Vec<Endpoint> {
        self.listeners
            .iter()
            .flatten()
            .map(|b| b.endpoint.clone())
            .collect()
    }

    // ── progress ─────────────────────────────────────────────────────────

    /// Poll every woken child and process what they reported, until quiet.
    pub fn progress(&mut self) {
        loop {
            let polled = self.children.poll_woken();
            let processed = self.process_events();
            if !polled && !processed {
                break;
            }
        }
    }

    fn process_events(&mut self) -> bool {
        let mut any = false;
        loop {
            let hs = self.handshaken.borrow_mut().pop_front();
            if let Some(h) = hs {
                any = true;
                self.on_handshaken(h);
                continue;
            }
            let Some(ev) = self.events.pop() else {
                break;
            };
            any = true;
            match ev {
                Event::Accepted { listener, stream } => {
                    self.start_handshake(Origin::Listener(listener), stream, false);
                }
                Event::Connected { connector, stream } => {
                    self.start_handshake(Origin::Connector(connector), stream, true);
                }
                Event::ConnectFailed { connector, reason } => {
                    if let Some(Some(c)) = self.connectors.get(connector) {
                        self.emit(SocketEvent::ConnectFailed {
                            endpoint: c.endpoint.clone(),
                            reason,
                        });
                    }
                }
                Event::InprocOffer { listener, offer } => self.on_inproc_offer(listener, offer),
                Event::Readable(id) => self.on_readable(id),
                Event::Writable(_) => self.wakeups.wake_user(),
                Event::Closed(id, reason) => self.on_closed(id, &reason),
                Event::Tick => self.on_tick(),
                Event::LingerExpired => self.linger_expired = true,
            }
        }
        any
    }

    fn start_handshake(&mut self, origin: Origin, stream: Stream, outgoing: bool) {
        if self.closing {
            return;
        }
        let cfg = Rc::clone(&self.cfg);
        let options = Rc::clone(&self.options);
        let stream = Rc::new(stream);
        let done = Rc::clone(&self.handshaken);
        self.children.spawn(async move {
            let result = P::handshake(cfg, options, Rc::clone(&stream), outgoing).await;
            done.borrow_mut().push_back(Handshaken {
                origin,
                stream,
                result,
            });
        });
    }

    fn on_handshaken(&mut self, h: Handshaken<P>) {
        let endpoint = match h.origin {
            Origin::Connector(i) => self
                .connectors
                .get(i)
                .and_then(Option::as_ref)
                .map(|c| c.endpoint.clone()),
            Origin::Listener(i) => self
                .listeners
                .get(i)
                .and_then(Option::as_ref)
                .map(|b| b.endpoint.clone()),
            Origin::Attached => None,
        };
        match h.result {
            Ok((codec, mut info)) if !self.closing => {
                info.endpoint.clone_from(&endpoint);
                if let Origin::Connector(i) = h.origin
                    && let Some(Some(Connector {
                        routing_id: Some(id),
                        ..
                    })) = self.connectors.get(i)
                {
                    info.identity = Some(id.clone());
                }
                let id = self.add_wire_peer(h.origin, h.stream, codec, info);
                if let Origin::Connector(i) = h.origin
                    && let Some(Some(c)) = self.connectors.get_mut(i)
                {
                    c.peer = Some(id);
                }
                if let Some(ep) = endpoint {
                    self.emit(match h.origin {
                        Origin::Listener(_) => SocketEvent::Accepted(ep),
                        _ => SocketEvent::Connected(ep),
                    });
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!("handshake failed: {e}");
                if let Origin::Connector(i) = h.origin
                    && let Some(Some(c)) = self.connectors.get(i)
                {
                    self.emit(SocketEvent::ConnectFailed {
                        endpoint: c.endpoint.clone(),
                        reason: e.to_string(),
                    });
                    c.down.fire();
                }
            }
        }
    }

    fn alloc_peer_id(&mut self) -> PeerId {
        if let Some(index) = self.free_peers.pop() {
            let generation = self.generations[index as usize].wrapping_add(1);
            self.generations[index as usize] = generation;
            PeerId { index, generation }
        } else {
            let index = u32::try_from(self.peers.len()).expect("fewer than 2^32 peers");
            self.peers.push(None);
            self.generations.push(0);
            PeerId {
                index,
                generation: 0,
            }
        }
    }

    fn add_wire_peer(
        &mut self,
        origin: Origin,
        stream: Rc<Stream>,
        codec: P::Codec,
        info: PeerInfo,
    ) -> PeerId {
        let id = self.alloc_peer_id();
        let state = Rc::new(RefCell::new(PeerState::new(
            id,
            Outbound::Wire {
                codec,
                out: OutQueue::default(),
                writer_waker: None,
                hold: coalesce_threshold(&self.options),
                flushing: false,
            },
            self.options.recv_hwm,
            self.options.send_hwm,
            Rc::clone(&self.events),
            self.options.heartbeat_ivl.is_some(),
        )));
        state.borrow_mut().conflate = self.conflates();
        let reader = self.children.spawn(peer::reader_loop(
            Rc::clone(&state),
            Rc::clone(&stream),
            self.options.read_buffer_size(),
        ));
        let writer = self
            .children
            .spawn(peer::writer_loop(Rc::clone(&state), stream));
        self.install_peer(id, state, info, origin, [Some(reader), Some(writer)]);
        id
    }

    fn install_peer(
        &mut self,
        id: PeerId,
        state: Rc<RefCell<PeerState<P::Codec>>>,
        info: PeerInfo,
        origin: Origin,
        children: [Option<ChildId>; 2],
    ) {
        if !self.hooks.on_peer_up(id, &info) {
            // The pattern refused the peer (PAIR already has one).
            state.borrow_mut().close(CloseReason::Local);
            for child in children.into_iter().flatten() {
                self.children.cancel(child);
            }
            self.free_peers.push(id.index);
            if let Origin::Connector(c) = origin
                && let Some(Some(conn)) = self.connectors.get(c)
            {
                conn.down.fire();
            }
            // Drop the Closed event the refusal queued; the peer never existed.
            self.events
                .q
                .borrow_mut()
                .retain(|ev| !matches!(ev, Event::Closed(p, _) if *p == id));
            return;
        }
        {
            let mut st = state.borrow_mut();
            // Greetings bypass the HWM: they are what makes the peer useful
            // at all.
            for msg in self.greeting.iter().chain(&self.hooks.greeting()) {
                let _ = st.enqueue(msg);
            }
        }
        self.peers[id.index as usize] = Some(PeerEntry {
            state,
            info,
            children,
            origin,
            in_fair: false,
            closed: false,
        });
        self.live_peers += 1;
        self.lb.push(id);
        if self.heartbeat.is_none()
            && let Some(ivl) = self.options.heartbeat_ivl
        {
            self.heartbeat = Some(
                self.children
                    .spawn(driver::heartbeat_loop(ivl, Rc::clone(&self.events))),
            );
        }
        self.wakeups.wake_user();
    }

    fn on_inproc_offer(&mut self, listener: usize, offer: inproc::Offer) {
        if self.closing || !P::compatible(&self.cfg, offer.info.socket_type) {
            return;
        }
        let mut info = offer.info;
        info.endpoint = self
            .listeners
            .get(listener)
            .and_then(Option::as_ref)
            .map(|b| b.endpoint.clone());
        self.add_link_peer(Origin::Listener(listener), offer.link, info);
    }

    fn add_link_peer(&mut self, origin: Origin, link: inproc::Link, info: PeerInfo) -> PeerId {
        let id = self.alloc_peer_id();
        let (outbound, reader): (Outbound<P::Codec>, _) = match link {
            inproc::Link::Local { tx, rx } => (Outbound::Local(tx), Ok(rx)),
            inproc::Link::Remote { tx, rx } => (Outbound::Remote(tx), Err(rx)),
        };
        let state = Rc::new(RefCell::new(PeerState::new(
            id,
            outbound,
            self.options.recv_hwm,
            self.options.send_hwm,
            Rc::clone(&self.events),
            false,
        )));
        state.borrow_mut().conflate = self.conflates();
        let child = match reader {
            Ok(rx) => self
                .children
                .spawn(peer::local_reader_loop(Rc::clone(&state), rx)),
            Err(rx) => self
                .children
                .spawn(peer::remote_reader_loop(Rc::clone(&state), rx)),
        };
        self.install_peer(id, state, info, origin, [Some(child), None]);
        id
    }

    fn on_readable(&mut self, id: PeerId) {
        let eager = self.hooks.eager_inbound();
        let i = id.index as usize;
        let Some(entry) = self.peers.get_mut(i).and_then(Option::as_mut) else {
            return;
        };
        if self.generations[i] != id.generation {
            return;
        }
        entry.state.borrow_mut().readable_seen();
        if eager {
            let state = Rc::clone(&entry.state);
            loop {
                let msg = state.borrow_mut().pop_ready();
                let Some(msg) = msg else { break };
                if let Some(keep) = self.hooks.on_inbound(id, msg) {
                    self.inbox.push_back((id, keep));
                }
            }
        } else if !entry.in_fair {
            entry.in_fair = true;
            self.fair.push_back(id);
        }
        self.wakeups.wake_user();
    }

    fn on_closed(&mut self, id: PeerId, reason: &CloseReason) {
        let i = id.index as usize;
        if self.generations.get(i) != Some(&id.generation) {
            return;
        }
        let Some(entry) = self.peers.get_mut(i).and_then(Option::as_mut) else {
            return;
        };
        if entry.closed {
            return;
        }
        tracing::debug!("peer {} closed: {reason:?}", id.index);
        entry.closed = true;
        entry.state.borrow_mut().close(reason.clone());
        let children = entry.children;
        let origin = entry.origin;
        let endpoint = entry.info.endpoint.clone();
        let has_ready = !entry.state.borrow().ready.is_empty();
        for child in children.into_iter().flatten() {
            self.children.cancel(child);
        }
        self.lb.retain(|p| *p != id);
        self.live_peers -= 1;
        if let Some(entry) = self.peers[i].as_ref() {
            let info = entry.info.clone();
            self.hooks.on_peer_down(id, &info);
        }
        if let Origin::Connector(c) = origin
            && let Some(Some(conn)) = self.connectors.get_mut(c)
            && conn.peer == Some(id)
        {
            conn.peer = None;
            conn.down.fire();
        }
        if let Some(ep) = endpoint {
            self.emit(SocketEvent::Disconnected(ep));
        }
        if !has_ready {
            self.free_peer(id);
        }
        self.wakeups.wake_user();
    }

    fn free_peer(&mut self, id: PeerId) {
        let i = id.index as usize;
        if self.peers[i].take().is_some() {
            self.free_peers.push(id.index);
            self.hooks.on_peer_freed(id);
        }
    }

    fn on_tick(&mut self) {
        let Some(ivl) = self.options.heartbeat_ivl else {
            return;
        };
        let timeout = self.options.heartbeat_timeout.unwrap_or(ivl);
        let ttl = self.options.heartbeat_ttl.unwrap_or(ivl);
        let now = Instant::now();
        let mut dead = Vec::new();
        for entry in self.peers.iter().flatten() {
            if entry.closed {
                continue;
            }
            let mut st = entry.state.borrow_mut();
            let Some(mut last) = st.last_recv else {
                continue;
            };
            if std::mem::take(&mut st.recv_seen) {
                last = now;
                st.last_recv = Some(now);
            }
            let idle = now.saturating_duration_since(last);
            let limit = match &st.outbound {
                Outbound::Wire { codec, .. } => {
                    codec.peer_ttl().map_or(timeout, |t| t.min(timeout))
                }
                _ => timeout,
            };
            if st.ping_outstanding && idle >= ivl + limit {
                dead.push(st.id);
            } else if idle >= ivl && !st.ping_outstanding {
                st.ping_outstanding = true;
                st.enqueue_command(|codec, out| codec.encode_ping(ttl, out));
            }
        }
        for id in dead {
            self.on_closed(id, &CloseReason::HeartbeatTimeout);
        }
    }

    // ── endpoints ────────────────────────────────────────────────────────

    fn ensure_driver(this: &Shared<P, H>) {
        let mut core = this.borrow_mut();
        if !core.driver_running {
            core.driver_running = true;
            drop(core);
            crate::rt::spawn_detached(driver::Driver {
                core: Rc::clone(this),
            });
        }
    }

    /// Bind `endpoint` and start accepting. Returns the endpoint actually
    /// bound (with an OS-assigned TCP port resolved).
    pub async fn bind(this: &Shared<P, H>, endpoint: &str) -> Result<Endpoint> {
        let endpoint = Endpoint::parse(endpoint)?;
        let options = Rc::clone(&this.borrow().options);
        if let Endpoint::Inproc(name) = &endpoint {
            let info = {
                let core = this.borrow();
                PeerInfo {
                    endpoint: Some(endpoint.clone()),
                    ..PeerInfo::local(P::socket_type(&core.cfg), &core.options)
                }
            };
            let binding = inproc::bind(name, info)?;
            Self::ensure_driver(this);
            let mut core = this.borrow_mut();
            let index = core.listeners.len();
            let events = Rc::clone(&core.events);
            let child = core
                .children
                .spawn(driver::inproc_accept_loop(index, binding, events));
            core.listeners.push(Some(Bound {
                endpoint: endpoint.clone(),
                child,
            }));
            core.emit(SocketEvent::Bound(endpoint.clone()));
            return Ok(endpoint);
        }
        let (listener, bound) = match Listener::bind(&endpoint, &options).await {
            Ok(v) => v,
            Err(e) => {
                this.borrow().emit(SocketEvent::BindFailed {
                    endpoint: endpoint.clone(),
                    reason: e.to_string(),
                });
                return Err(e.into());
            }
        };
        Self::ensure_driver(this);
        let mut core = this.borrow_mut();
        let index = core.listeners.len();
        let events = Rc::clone(&core.events);
        let child = core
            .children
            .spawn(driver::accept_loop(index, listener, events, options));
        core.listeners.push(Some(Bound {
            endpoint: bound.clone(),
            child,
        }));
        core.emit(SocketEvent::Bound(bound.clone()));
        core.emit(SocketEvent::Listening(bound.clone()));
        Ok(bound)
    }

    /// Start dialing `endpoint`, reconnecting whenever the connection drops.
    ///
    /// Returns once dialing has started; the connection completes in the
    /// background. For inproc the binder must already exist.
    pub fn connect(this: &Shared<P, H>, endpoint: &str) -> Result<()> {
        let endpoint = Endpoint::parse(endpoint)?;
        Self::ensure_driver(this);
        let mut core = this.borrow_mut();
        if let Endpoint::Inproc(name) = &endpoint {
            let info = PeerInfo {
                endpoint: Some(endpoint.clone()),
                ..PeerInfo::local(P::socket_type(&core.cfg), &core.options)
            };
            let hwm = core.options.send_hwm;
            let (link, mut binder) = inproc::connect(name, info, hwm)?;
            if !P::compatible(&core.cfg, binder.socket_type) {
                return Err(MonocoqueError::Protocol(format!(
                    "incompatible inproc peer socket type {}",
                    binder.socket_type
                )));
            }
            binder.endpoint = Some(endpoint.clone());
            if let Some(id) = core.connect_routing_id.take() {
                binder.identity = Some(id);
            }
            core.add_link_peer(Origin::Attached, link, binder);
            core.emit(SocketEvent::Connected(endpoint));
            return Ok(());
        }
        let index = core.connectors.len();
        let down = Rc::new(Signal::default());
        let events = Rc::clone(&core.events);
        let options = Rc::clone(&core.options);
        let child = core.children.spawn(driver::connect_loop(
            index,
            endpoint.clone(),
            events,
            Rc::clone(&down),
            options,
        ));
        let routing_id = core.connect_routing_id.take();
        core.connectors.push(Some(Connector {
            endpoint,
            child,
            down,
            peer: None,
            routing_id,
        }));
        drop(core);
        // Run the connector once now so the dial is submitted immediately.
        this.borrow_mut().progress();
        Ok(())
    }

    /// Stop dialing `endpoint` and drop its connection.
    pub fn disconnect(&mut self, endpoint: &str) -> Result<()> {
        let endpoint = Endpoint::parse(endpoint)?;
        let Some(i) = self
            .connectors
            .iter()
            .position(|c| c.as_ref().is_some_and(|c| c.endpoint == endpoint))
        else {
            return Err(MonocoqueError::InvalidEndpoint(format!(
                "{endpoint} is not connected"
            )));
        };
        let conn = self.connectors[i].take().expect("position found it");
        self.children.cancel(conn.child);
        if let Some(id) = conn.peer {
            self.on_closed(id, &CloseReason::Local);
        }
        Ok(())
    }

    /// Stop accepting on `endpoint`. Established peers stay connected.
    pub fn unbind(&mut self, endpoint: &str) -> Result<()> {
        let endpoint = Endpoint::parse(endpoint)?;
        let Some(i) = self
            .listeners
            .iter()
            .position(|b| b.as_ref().is_some_and(|b| b.endpoint == endpoint))
        else {
            return Err(MonocoqueError::InvalidEndpoint(format!(
                "{endpoint} is not bound"
            )));
        };
        let bound = self.listeners[i].take().expect("position found it");
        self.children.cancel(bound.child);
        Ok(())
    }

    /// Handshake an already-connected stream and add it as a peer.
    pub async fn attach(this: &Shared<P, H>, stream: Stream) -> Result<PeerId> {
        let (cfg, options) = {
            let core = this.borrow();
            (Rc::clone(&core.cfg), Rc::clone(&core.options))
        };
        stream.configure(&options)?;
        let stream = Rc::new(stream);
        let (codec, info) = P::handshake(cfg, options, Rc::clone(&stream), false).await?;
        Self::ensure_driver(this);
        let mut core = this.borrow_mut();
        let id = core.add_wire_peer(Origin::Attached, stream, codec, info);
        core.progress();
        Ok(id)
    }

    /// Like [`attach`](Self::attach) for the connecting side of a stream.
    pub async fn attach_outgoing(this: &Shared<P, H>, stream: Stream) -> Result<PeerId> {
        let (cfg, options) = {
            let core = this.borrow();
            (Rc::clone(&core.cfg), Rc::clone(&core.options))
        };
        stream.configure(&options)?;
        let stream = Rc::new(stream);
        let (codec, info) = P::handshake(cfg, options, Rc::clone(&stream), true).await?;
        Self::ensure_driver(this);
        let mut core = this.borrow_mut();
        let id = core.add_wire_peer(Origin::Attached, stream, codec, info);
        core.progress();
        Ok(id)
    }

    // ── receive ──────────────────────────────────────────────────────────

    /// Take the next message from any peer, fair-queued across peers.
    pub fn try_recv(&mut self) -> Option<(PeerId, Msg)> {
        self.progress();
        self.take_ready()
    }

    fn take_ready(&mut self) -> Option<(PeerId, Msg)> {
        if let Some(m) = self.inbox.pop_front() {
            return Some(m);
        }
        while let Some(id) = self.fair.pop_front() {
            let i = id.index as usize;
            let Some(entry) = self.peers.get_mut(i).and_then(Option::as_mut) else {
                continue;
            };
            let (msg, more) = {
                let mut st = entry.state.borrow_mut();
                let msg = st.pop_ready();
                (msg, !st.ready.is_empty())
            };
            if more {
                self.fair.push_back(id);
            } else {
                entry.in_fair = false;
                if entry.closed {
                    self.free_peer(id);
                }
            }
            let Some(msg) = msg else { continue };
            match self.hooks.on_inbound(id, msg) {
                Some(msg) => return Some((id, msg)),
                None => continue,
            }
        }
        None
    }

    /// Poll for the next message from any peer.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<(PeerId, Msg)> {
        loop {
            self.progress();
            if let Some(m) = self.take_ready() {
                return Poll::Ready(m);
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// Like [`poll_recv`](Self::poll_recv), writing the message straight into
    /// `slot` so it is not moved through every layer of the calling future.
    pub fn poll_recv_into(&mut self, cx: &mut Context<'_>, slot: &mut Msg) -> Poll<PeerId> {
        loop {
            self.progress();
            if let Some((id, msg)) = self.take_ready() {
                *slot = msg;
                return Poll::Ready(id);
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// Poll for the next message from `id` only. `Ready(None)` once that peer
    /// is gone and has nothing left.
    pub fn poll_recv_from(&mut self, cx: &mut Context<'_>, id: PeerId) -> Poll<Option<Msg>> {
        let mut slot = Msg::new();
        self.poll_recv_from_into(cx, id, &mut slot)
            .map(|got| got.then_some(slot))
    }

    /// Like [`poll_recv_from`](Self::poll_recv_from), writing into `slot`.
    /// `Ready(false)` once the peer is gone and has nothing left.
    pub fn poll_recv_from_into(
        &mut self,
        cx: &mut Context<'_>,
        id: PeerId,
        slot: &mut Msg,
    ) -> Poll<bool> {
        loop {
            self.progress();
            let i = id.index as usize;
            let Some(entry) = self.entry(id) else {
                return Poll::Ready(false);
            };
            let (msg, empty, closed) = {
                let mut st = entry.state.borrow_mut();
                let msg = st.pop_ready();
                (msg, st.ready.is_empty(), entry.closed)
            };
            if let Some(msg) = msg {
                if let Some(msg) = self.hooks.on_inbound(id, msg) {
                    *slot = msg;
                    return Poll::Ready(true);
                }
                continue;
            }
            if closed && empty {
                if let Some(e) = self.peers[i].as_ref()
                    && !e.in_fair
                {
                    self.free_peer(id);
                }
                return Poll::Ready(false);
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// `true` if a receive would return without waiting.
    pub fn has_ready(&mut self) -> bool {
        self.progress();
        !self.inbox.is_empty()
            || self.fair.iter().any(|id| {
                self.entry(*id)
                    .is_some_and(|e| !e.state.borrow().ready.is_empty())
            })
    }

    /// Wait until a message is queued, without taking it.
    pub fn poll_readable(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            if self.has_ready() {
                return Poll::Ready(());
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// `true` if a fair-queued receive would return a message now. Unlike
    /// [`has_ready`](Self::has_ready) this runs the inbound hook on the next
    /// message, so one a filter would drop (SUB) does not count. A message
    /// that passes stays queued, at the front, for the next receive.
    pub fn peek_ready(&mut self) -> bool {
        self.progress();
        if !self.inbox.is_empty() {
            return true;
        }
        if let Some(m) = self.take_ready() {
            self.inbox.push_front(m);
            return true;
        }
        false
    }

    /// `true` if `id` has a message queued.
    pub fn has_ready_from(&mut self, id: PeerId) -> bool {
        self.progress();
        self.entry(id)
            .is_some_and(|e| !e.state.borrow().ready.is_empty())
    }

    /// Wait until some live peer has send room, without sending.
    pub fn poll_writable(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            if self.any_writable() {
                return Poll::Ready(());
            }
            if self.watch_send_room(cx) {
                continue;
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// Ask the inproc peers that are full to wake `cx` once their receiver
    /// makes room. Wire peers report room through their writer and need
    /// nothing here. Returns `true` if some peer has room by now, in which
    /// case the caller must retry rather than park.
    pub fn watch_send_room(&self, cx: &mut Context<'_>) -> bool {
        let mut room = false;
        for id in &self.lb {
            if let Some(e) = self.entry(*id)
                && !e.closed
            {
                room |= e.state.borrow().watch_send_room(cx);
            }
        }
        room
    }

    /// Park the user task for a readiness wait spanning several sockets.
    /// Returns `false` if work arrived meanwhile and the caller should look
    /// again before returning `Pending`. The caller must
    /// [`Wakeups::unpark_user`] once it stops waiting.
    pub fn park_poll(&self, cx: &mut Context<'_>) -> bool {
        self.park(cx)
    }

    /// Park the user task. Returns `false` if work arrived meanwhile and the
    /// caller should look again instead of returning `Pending`.
    fn park(&self, cx: &mut Context<'_>) -> bool {
        self.wakeups.park_user(cx.waker());
        if self.wakeups.has_woken() || !self.events.q.borrow().is_empty() {
            return false;
        }
        true
    }

    // ── send ─────────────────────────────────────────────────────────────

    /// Send `msg` to the next peer with room, round-robin. Waits while no peer
    /// has room (or none is connected). Returns the peer it went to.
    pub fn poll_send_lb(
        &mut self,
        cx: &mut Context<'_>,
        msg: &mut Option<Msg>,
    ) -> Poll<Result<PeerId>> {
        self.poll_send_lb_prefixed(cx, &[], msg)
    }

    /// [`poll_send_lb`](Self::poll_send_lb) with `prefix` frames sent ahead
    /// of the message (an envelope), without rebuilding the message.
    pub fn poll_send_lb_prefixed(
        &mut self,
        cx: &mut Context<'_>,
        prefix: &[Bytes],
        msg: &mut Option<Msg>,
    ) -> Poll<Result<PeerId>> {
        loop {
            self.progress();
            if let Some(id) = self.pick_writable() {
                let m = msg.take().expect("polled after completion");
                let result = self.enqueue_to(id, prefix, m);
                self.progress();
                return Poll::Ready(result.map(|()| id));
            }
            if self.closing {
                return Poll::Ready(Err(MonocoqueError::Closed));
            }
            // A peer on another thread can make room between the pick above
            // and here; parking then would wait for a wakeup never armed.
            if self.watch_send_room(cx) {
                continue;
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    fn pick_writable(&mut self) -> Option<PeerId> {
        let n = self.lb.len();
        for k in 0..n {
            let i = (self.lb_cursor + k) % n;
            let id = self.lb[i];
            if self
                .entry(id)
                .is_some_and(|e| !e.closed && e.state.borrow().writable())
            {
                self.lb_cursor = (i + 1) % n;
                return Some(id);
            }
        }
        None
    }

    fn enqueue_to(&mut self, id: PeerId, prefix: &[Bytes], msg: Msg) -> Result<()> {
        let Some(entry) = self.entry(id) else {
            return Err(MonocoqueError::HostUnreachable);
        };
        entry.state.borrow_mut().enqueue_prefixed(prefix, msg)
    }

    /// Queue `msg` toward `id` without waiting.
    pub fn send_to(&mut self, id: PeerId, msg: Msg) -> SendTo {
        self.progress();
        let Some(entry) = self.entry(id) else {
            return SendTo::Gone(msg);
        };
        if entry.closed {
            return SendTo::Gone(msg);
        }
        if !entry.state.borrow().writable() {
            return SendTo::Full(msg);
        }
        let sent = entry.state.borrow_mut().enqueue_owned(msg).is_ok();
        self.progress();
        if sent {
            SendTo::Sent
        } else {
            SendTo::Gone(Msg::new())
        }
    }

    /// Wait until `id` has send room, then queue `msg` toward it.
    pub fn poll_send_to(
        &mut self,
        cx: &mut Context<'_>,
        id: PeerId,
        msg: &mut Option<Msg>,
    ) -> Poll<Result<()>> {
        self.poll_send_to_prefixed(cx, id, &[], msg)
    }

    /// [`poll_send_to`](Self::poll_send_to) with `prefix` frames ahead of
    /// the message.
    pub fn poll_send_to_prefixed(
        &mut self,
        cx: &mut Context<'_>,
        id: PeerId,
        prefix: &[Bytes],
        msg: &mut Option<Msg>,
    ) -> Poll<Result<()>> {
        loop {
            self.progress();
            let Some(entry) = self.entry(id) else {
                return Poll::Ready(Err(MonocoqueError::HostUnreachable));
            };
            if entry.closed {
                return Poll::Ready(Err(MonocoqueError::HostUnreachable));
            }
            if entry.state.borrow().writable() {
                let m = msg.take().expect("polled after completion");
                let result = entry.state.borrow_mut().enqueue_prefixed(prefix, m);
                self.progress();
                return Poll::Ready(result);
            }
            if entry.state.borrow().watch_send_room(cx) {
                continue;
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// Send `msg` to every live peer `select` accepts, skipping peers at
    /// their send HWM. Encodes once for every peer that can share an encoding.
    /// Returns how many peers it was queued to.
    pub fn fanout(
        &mut self,
        msg: &Msg,
        mut select: impl FnMut(PeerId, &PeerInfo) -> bool,
    ) -> usize {
        self.progress();
        let mut shared: Option<Bytes> = None;
        let mut sent = 0;
        for k in 0..self.lb.len() {
            let id = self.lb[k];
            let chosen = self
                .entry(id)
                .is_some_and(|e| !e.closed && select(id, &e.info));
            if chosen && self.fanout_one(id, msg, &mut shared) {
                sent += 1;
            }
        }
        self.progress();
        sent
    }

    /// Send `msg` to each peer in `ids` that is live and has room. Like
    /// [`fanout`](Self::fanout) for a caller that already knows the targets.
    pub fn fanout_to(&mut self, msg: &Msg, ids: &[PeerId]) -> usize {
        self.progress();
        let mut shared: Option<Bytes> = None;
        let sent = ids
            .iter()
            .filter(|&&id| self.fanout_one(id, msg, &mut shared))
            .count();
        self.progress();
        sent
    }

    /// Queue `msg` toward `id` for a fan-out, reusing `shared` as the one
    /// encoding every sharing peer gets.
    fn fanout_one(&mut self, id: PeerId, msg: &Msg, shared: &mut Option<Bytes>) -> bool {
        let state = match self.entry(id) {
            Some(entry) if !entry.closed => Rc::clone(&entry.state),
            _ => return false,
        };
        let mut st = state.borrow_mut();
        if !st.writable() {
            return false;
        }
        let share = matches!(&st.outbound, Outbound::Wire { codec, hold: None, .. } if codec.shares_encoding());
        if share {
            let wire = shared.get_or_insert_with(|| {
                // Encode into a large shared region so successive messages are
                // carved from one allocation: every peer still holds earlier
                // encodings, so a buffer sized to one message would be
                // reallocated on every publish.
                if self.shared_wire.capacity() < 1024 {
                    self.shared_wire.reserve(SHARED_WIRE_REGION);
                }
                P::encode_shared(&self.cfg, msg, &mut self.shared_wire);
                self.shared_wire.split().freeze()
            });
            st.enqueue_wire(wire.clone());
            true
        } else {
            st.enqueue(msg).is_ok()
        }
    }

    /// Wait until everything queued toward `id` has been written (or the
    /// peer is gone). A send that waits on this is eager: once it returns,
    /// the bytes are in the kernel and need no further action from the caller.
    pub fn poll_written(&mut self, cx: &mut Context<'_>, id: PeerId) -> Poll<()> {
        loop {
            self.progress();
            match self.entry(id) {
                Some(e) if !e.closed && e.state.borrow().write_pending() => {}
                _ => return Poll::Ready(()),
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// Queue a control message (a subscription change) to every live peer,
    /// ignoring the send HWM: dropping one would silently change what the
    /// peer delivers.
    pub fn broadcast_control(&mut self, msg: &Msg) {
        self.progress();
        for k in 0..self.lb.len() {
            let id = self.lb[k];
            if let Some(entry) = self.entry(id)
                && !entry.closed
            {
                let _ = entry.state.borrow_mut().enqueue(msg);
            }
        }
        self.progress();
    }

    /// Close one peer's connection after its queued messages are written.
    pub fn close_peer(&mut self, id: PeerId) {
        if let Some(entry) = self.entry(id) {
            entry.state.borrow_mut().close_after_drain();
        }
        self.progress();
    }

    /// Release writes held by `write_coalescing` on every peer.
    pub fn flush(&mut self) {
        for entry in self.peers.iter().flatten() {
            entry.state.borrow_mut().flush();
        }
        self.progress();
    }

    /// Wait until every peer's queued messages have been written.
    pub fn poll_flushed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            self.flush();
            let drained = self
                .peers
                .iter()
                .flatten()
                .all(|e| e.closed || e.state.borrow().drained());
            if drained {
                return Poll::Ready(());
            }
            if self.park(cx) {
                return Poll::Pending;
            }
        }
    }

    /// Messages queued toward `id` and not yet written.
    pub fn pending_out(&self, id: PeerId) -> usize {
        self.entry(id).map_or(0, |e| e.state.borrow().pending_out())
    }

    /// `true` if some live peer could take a message now.
    pub fn any_writable(&mut self) -> bool {
        self.progress();
        self.lb
            .iter()
            .any(|id| self.entry(*id).is_some_and(|e| e.state.borrow().writable()))
    }

    // ── close ────────────────────────────────────────────────────────────

    /// Begin closing: stop accepting and dialing, stop reading, and let the
    /// driver drain queued messages for up to `linger`.
    pub fn close(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        for b in self.listeners.iter_mut().filter_map(Option::take) {
            self.children.cancel(b.child);
        }
        for c in self.connectors.iter_mut().filter_map(Option::take) {
            self.children.cancel(c.child);
        }
        if let Some(hb) = self.heartbeat.take() {
            self.children.cancel(hb);
        }
        for entry in self.peers.iter_mut().flatten() {
            if let Some(reader) = entry.children[0].take() {
                self.children.cancel(reader);
            }
            entry.state.borrow_mut().flush();
        }
        match self.options.linger {
            Some(d) if d.is_zero() => self.linger_expired = true,
            Some(d) => {
                self.children
                    .spawn(driver::linger_timer(d, Rc::clone(&self.events)));
            }
            None => {}
        }
        self.wakeups.wake_driver();
    }

    fn finished(&self) -> bool {
        if !self.closing {
            return false;
        }
        self.linger_expired
            || self
                .peers
                .iter()
                .flatten()
                .all(|e| e.closed || e.state.borrow().drained())
    }

    fn shutdown_now(&mut self) {
        let ids: Vec<PeerId> = self.lb.clone();
        for id in ids {
            self.on_closed(id, &CloseReason::Local);
        }
        self.children = Children::new(Arc::clone(&self.wakeups));
    }
}

/// Run `f` against the core as a future, parking the user task on `Pending`
/// and unparking it when the future completes or is dropped, so wakeups never
/// strand on a task that stopped listening.
pub const fn op<'a, P, H, T, F>(core: &'a Shared<P, H>, f: F) -> Op<'a, P, H, F>
where
    P: Protocol,
    H: Hooks,
    F: FnMut(&mut SocketCore<P, H>, &mut Context<'_>) -> Poll<T> + Unpin + 'a,
{
    Op { core, f }
}

/// The future returned by [`op`].
pub struct Op<'a, P: Protocol, H: Hooks, F> {
    core: &'a Shared<P, H>,
    f: F,
}

impl<P, H, T, F> Future for Op<'_, P, H, F>
where
    P: Protocol,
    H: Hooks,
    F: FnMut(&mut SocketCore<P, H>, &mut Context<'_>) -> Poll<T> + Unpin,
{
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = &mut *self;
        this.core.borrow().wakeups.enter();
        let result = (this.f)(&mut this.core.borrow_mut(), cx);
        let core = this.core.borrow();
        if result.is_ready() {
            core.wakeups.unpark_user();
        }
        core.wakeups.exit();
        result
    }
}

impl<P: Protocol, H: Hooks, F> Drop for Op<'_, P, H, F> {
    fn drop(&mut self) {
        if let Ok(core) = self.core.try_borrow() {
            core.wakeups.unpark_user();
        }
    }
}
