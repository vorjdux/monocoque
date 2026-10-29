//! The ZeroMQ patterns: what each socket type does on send and receive.

use super::{Kind, Socket};
use crate::session::SocketType;
use bytes::Bytes;
use monocoque_core::error::{MonocoqueError, Result};
use monocoque_core::msg::{Msg, MsgRef};
use monocoque_core::mux::{Hooks, PeerId, PeerInfo, SendTo};
use monocoque_core::options::SocketOptions;
use monocoque_core::subscription::{
    Subscribed, SubscriptionEvent, SubscriptionIndex, SubscriptionTrie, Unsubscribed,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::collections::hash_map::RandomState;

/// Hooks for patterns that need none.
#[derive(Default)]
pub struct Plain;

impl Hooks for Plain {}

macro_rules! kind {
    ($(#[$doc:meta])* $name:ident, $ty:expr, $hooks:ty, $ext:ty) => {
        $(#[$doc])*
        pub struct $name;
        impl Kind for $name {
            const TYPE: SocketType = $ty;
            type Hooks = $hooks;
            type Ext = $ext;
            fn hooks(options: &SocketOptions) -> $hooks {
                <$hooks>::from_options(options)
            }
        }
    };
}

trait FromOptions {
    fn from_options(options: &SocketOptions) -> Self;
}

impl FromOptions for Plain {
    fn from_options(_: &SocketOptions) -> Self {
        Self
    }
}

impl<K: Kind> Socket<K> {
    async fn recv_fair(&mut self) -> Result<MsgRef<'_>> {
        self.next_into_slot().await?;
        Ok(self.lend())
    }
}

// ── PAIR ─────────────────────────────────────────────────────────────────

/// PAIR keeps exactly one peer; later connections are refused.
#[derive(Default)]
pub struct PairHooks {
    peer: Option<PeerId>,
}

impl FromOptions for PairHooks {
    fn from_options(_: &SocketOptions) -> Self {
        Self::default()
    }
}

impl Hooks for PairHooks {
    fn on_peer_up(&mut self, id: PeerId, _info: &PeerInfo) -> bool {
        if self.peer.is_some() {
            return false;
        }
        self.peer = Some(id);
        true
    }

    fn on_peer_down(&mut self, id: PeerId, _info: &PeerInfo) {
        if self.peer == Some(id) {
            self.peer = None;
        }
    }
}

kind!(
    /// PAIR: one exclusive bidirectional peer.
    Pair, SocketType::Pair, PairHooks, ()
);

impl Socket<Pair> {
    /// Send to the peer, waiting for one if none is connected.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        self.send_lb(msg.into()).await.map(drop)
    }

    /// Receive the next message.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_fair().await
    }

    /// Receive the next message and keep it.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_fair().await.map(MsgRef::take)
    }
}

// ── DEALER / PUSH / PULL ─────────────────────────────────────────────────

kind!(
    /// DEALER: round-robin sends, fair-queued receives.
    Dealer, SocketType::Dealer, Plain, ()
);

impl Socket<Dealer> {
    /// Send to the next peer with room, waiting while none has.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        self.send_lb(msg.into()).await.map(drop)
    }

    /// Receive the next message from any peer.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_fair().await
    }

    /// Receive the next message and keep it.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_fair().await.map(MsgRef::take)
    }
}

kind!(
    /// PUSH: round-robin sends to PULL peers; cannot receive.
    Push, SocketType::Push, Plain, ()
);

impl Socket<Push> {
    /// Send to the next peer with room, waiting while none has.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        self.send_lb(msg.into()).await.map(drop)
    }
}

kind!(
    /// PULL: fair-queued receives from PUSH peers; cannot send.
    Pull, SocketType::Pull, Plain, ()
);

impl Socket<Pull> {
    /// Receive the next message from any peer.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_fair().await
    }

    /// Receive the next message and keep it.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_fair().await.map(MsgRef::take)
    }
}

// ── ROUTER ───────────────────────────────────────────────────────────────

/// ROUTER's routing table. Identities are chosen by peers, so the map uses the
/// per-process randomly seeded SipHash to resist hash flooding.
pub struct RouterHooks {
    by_identity: HashMap<Bytes, PeerId, RandomState>,
    identity_of: HashMap<PeerId, Bytes, RandomState>,
    next_generated: u32,
    handover: bool,
}

impl FromOptions for RouterHooks {
    fn from_options(options: &SocketOptions) -> Self {
        Self {
            by_identity: HashMap::default(),
            identity_of: HashMap::default(),
            next_generated: rand::random(),
            handover: options.router_handover,
        }
    }
}

impl RouterHooks {
    fn generate(&mut self) -> Bytes {
        // libzmq's form: a zero byte, then a 32-bit counter. Peer-chosen
        // identities may not start with zero, so the two never collide.
        self.next_generated = self.next_generated.wrapping_add(1);
        let mut id = [0u8; 5];
        id[1..].copy_from_slice(&self.next_generated.to_be_bytes());
        Bytes::copy_from_slice(&id)
    }
}

impl Hooks for RouterHooks {
    fn on_peer_up(&mut self, id: PeerId, info: &PeerInfo) -> bool {
        let identity = match info.identity.clone() {
            Some(identity) if identity.first() != Some(&0) => identity,
            _ => self.generate(),
        };
        if let Some(&old) = self.by_identity.get(&identity) {
            if !self.handover {
                return false;
            }
            self.identity_of.remove(&old);
        }
        self.by_identity.insert(identity.clone(), id);
        self.identity_of.insert(id, identity);
        true
    }

    fn on_peer_down(&mut self, id: PeerId, _info: &PeerInfo) {
        if let Some(identity) = self.identity_of.remove(&id)
            && self.by_identity.get(&identity) == Some(&id)
        {
            self.by_identity.remove(&identity);
        }
    }
}

kind!(
    /// ROUTER: receives are prefixed with the sender's identity; sends are
    /// routed by their first frame.
    Router, SocketType::Router, RouterHooks, ()
);

impl Socket<Router> {
    /// Receive the next message, prefixed with its sender's identity frame.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        loop {
            let peer = self.next_into_slot().await?;
            let identity = self.core.borrow().hooks().identity_of.get(&peer).cloned();
            if let Some(identity) = identity {
                self.slot_mut().push_front(identity);
                return Ok(self.lend());
            }
        }
    }

    /// Receive and keep the next message.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv().await.map(MsgRef::take)
    }

    /// Route `msg` by its first frame. An unknown identity is dropped, or
    /// fails with `HostUnreachable` under `router_mandatory`; a peer at its
    /// HWM drops the message, or fails with `Again` under `router_mandatory`.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        let mut msg = msg.into();
        let mandatory = self.core.borrow().options().router_mandatory;
        let Some(identity) = msg.pop_front() else {
            return Err(MonocoqueError::InvalidRoutingId);
        };
        let peer = self
            .core
            .borrow()
            .hooks()
            .by_identity
            .get(&identity)
            .copied();
        let Some(peer) = peer else {
            return if mandatory {
                Err(MonocoqueError::HostUnreachable)
            } else {
                Ok(())
            };
        };
        match self.send_to_now(peer, msg) {
            SendTo::Sent => {
                self.wait_written(peer).await;
                Ok(())
            }
            SendTo::Full(_) if mandatory => Err(MonocoqueError::Again),
            SendTo::Gone(_) if mandatory => Err(MonocoqueError::HostUnreachable),
            SendTo::Full(_) | SendTo::Gone(_) => Ok(()),
        }
    }

    /// The identity a connected peer is routed by.
    #[must_use]
    pub fn identity_of(&self, peer: PeerId) -> Option<Bytes> {
        self.core.borrow().hooks().identity_of.get(&peer).cloned()
    }

    /// Identities of all connected peers.
    #[must_use]
    pub fn identities(&self) -> Vec<Bytes> {
        self.core
            .borrow()
            .hooks()
            .by_identity
            .keys()
            .cloned()
            .collect()
    }
}

// ── REQ / REP ────────────────────────────────────────────────────────────

/// REQ's place in its send/receive alternation.
#[derive(Default)]
pub struct ReqState {
    waiting_on: Option<PeerId>,
    request_id: u32,
}

kind!(
    /// REQ: strict send-then-receive with one peer per request.
    Req, SocketType::Req, Plain, ReqState
);

impl Socket<Req> {
    /// Send a request. Fails with `InvalidState` while a reply is pending,
    /// unless `req_relaxed` lets a new request abandon the old one.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        let (relaxed, correlate) = {
            let core = self.core.borrow();
            (core.options().req_relaxed, core.options().req_correlate)
        };
        if self.ext.waiting_on.is_some() && !relaxed {
            return Err(MonocoqueError::InvalidState(
                "REQ must receive a reply before sending again",
            ));
        }
        let msg = msg.into();
        let peer = if correlate {
            self.ext.request_id = self.ext.request_id.wrapping_add(1);
            let id = Bytes::copy_from_slice(&self.ext.request_id.to_be_bytes());
            self.send_lb_prefixed(&[id, Bytes::new()], msg).await?
        } else {
            self.send_lb_prefixed(&[Bytes::new()], msg).await?
        };
        self.ext.waiting_on = Some(peer);
        Ok(())
    }

    /// Receive the reply to the last request.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        let Some(peer) = self.ext.waiting_on else {
            return Err(MonocoqueError::InvalidState(
                "REQ must send a request before receiving",
            ));
        };
        let correlate = self.core.borrow().options().req_correlate;
        loop {
            if !self.next_from_into_slot(peer).await? {
                self.ext.waiting_on = None;
                return Err(MonocoqueError::PeerDisconnected(
                    "the REQ peer went away before replying".into(),
                ));
            }
            let request_id = self.ext.request_id.to_be_bytes();
            let msg = self.slot_mut();
            if correlate && msg.pop_front().as_deref() != Some(&request_id[..]) {
                continue;
            }
            // A reply without its empty delimiter is malformed: drop it.
            if msg.pop_front().is_some_and(|d| d.is_empty()) {
                self.ext.waiting_on = None;
                return Ok(self.lend());
            }
        }
    }

    /// Receive and keep the reply.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv().await.map(MsgRef::take)
    }
}

/// REP's pending reply envelope.
#[derive(Default)]
pub struct RepState {
    pending: Option<(PeerId, SmallVec<[Bytes; 4]>)>,
}

kind!(
    /// REP: strict receive-then-reply, routing each reply to its requester.
    Rep, SocketType::Rep, Plain, RepState
);

impl Socket<Rep> {
    /// Receive the next request. Fails with `InvalidState` while a reply is
    /// owed.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        if self.ext.pending.is_some() {
            return Err(MonocoqueError::InvalidState(
                "REP must reply before receiving again",
            ));
        }
        loop {
            let peer = self.next_into_slot().await?;
            // Peel the routing envelope: every frame up to and including the
            // first empty one. A request without a delimiter is malformed.
            let mut envelope: SmallVec<[Bytes; 4]> = SmallVec::new();
            let mut found = false;
            let msg = self.slot_mut();
            while let Some(frame) = msg.pop_front() {
                let empty = frame.is_empty();
                envelope.push(frame);
                if empty {
                    found = true;
                    break;
                }
            }
            if found {
                self.ext.pending = Some((peer, envelope));
                return Ok(self.lend());
            }
        }
    }

    /// Receive and keep the next request.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv().await.map(MsgRef::take)
    }

    /// Reply to the last request. Fails with `InvalidState` if none is owed. A
    /// requester that has gone away loses its reply silently.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        let Some((peer, envelope)) = self.ext.pending.take() else {
            return Err(MonocoqueError::InvalidState(
                "REP must receive a request before replying",
            ));
        };
        match self
            .send_to_waiting_prefixed(peer, &envelope, msg.into())
            .await
        {
            Err(MonocoqueError::HostUnreachable) => Ok(()),
            other => other,
        }
    }
}

// ── PUB / XPUB ───────────────────────────────────────────────────────────

/// Subscription state of a publisher.
pub struct PubHooks {
    index: SubscriptionIndex,
    xpub: bool,
    verbose: bool,
    verbose_unsubs: bool,
}

impl PubHooks {
    fn new(options: &SocketOptions, xpub: bool) -> Self {
        Self {
            index: SubscriptionIndex::new(),
            xpub,
            verbose: options.xpub_verbose,
            verbose_unsubs: options.xsub_verbose_unsubs,
        }
    }

    /// Distinct prefixes peer `id` holds.
    #[must_use]
    pub fn prefixes_of(&self, id: PeerId) -> usize {
        self.index.prefixes_of(id.as_u64())
    }
}

impl FromOptions for PubHooks {
    fn from_options(options: &SocketOptions) -> Self {
        Self::new(options, false)
    }
}

/// XPUB's hooks: PUB's, plus subscription messages are delivered to the user.
pub struct XPubHooks(PubHooks);

impl FromOptions for XPubHooks {
    fn from_options(options: &SocketOptions) -> Self {
        Self(PubHooks::new(options, true))
    }
}

impl PubHooks {
    fn apply(&mut self, id: PeerId, msg: Msg) -> Option<Msg> {
        // Slice the subscription out of the received frame: a refcount, no
        // copy (an inline frame of at most 64 bytes is the only one copied).
        let frame = msg.frame_bytes(0)?;
        let kind = *frame.first()?;
        let prefix = frame.slice(1..);
        match kind {
            1 => {
                let r = self.index.subscribe(id.as_u64(), prefix);
                let report = self.xpub
                    && (r == Subscribed::FirstForPrefix
                        || (self.verbose && r != Subscribed::Rejected));
                report.then_some(msg)
            }
            0 => {
                let r = self.index.unsubscribe(id.as_u64(), &prefix);
                let report = self.xpub
                    && (r == Unsubscribed::LastForPrefix
                        || (self.verbose_unsubs && r != Unsubscribed::NotSubscribed));
                report.then_some(msg)
            }
            // Anything else from a subscriber is user data, which only XPUB
            // passes on.
            _ => self.xpub.then_some(msg),
        }
    }

    fn drop_peer(&mut self, id: PeerId) -> Vec<Bytes> {
        self.index.remove_peer(id.as_u64())
    }
}

impl Hooks for PubHooks {
    fn eager_inbound(&self) -> bool {
        true
    }

    fn on_inbound(&mut self, id: PeerId, msg: Msg) -> Option<Msg> {
        self.apply(id, msg)
    }

    fn on_peer_down(&mut self, id: PeerId, _info: &PeerInfo) {
        self.drop_peer(id);
    }
}

impl Hooks for XPubHooks {
    fn eager_inbound(&self) -> bool {
        true
    }

    fn on_inbound(&mut self, id: PeerId, msg: Msg) -> Option<Msg> {
        self.0.apply(id, msg)
    }

    fn on_peer_down(&mut self, id: PeerId, _info: &PeerInfo) {
        self.0.drop_peer(id);
    }
}

kind!(
    /// PUB: each message goes to every subscriber whose subscriptions match
    /// its first frame; cannot receive.
    Pub, SocketType::Pub, PubHooks, PubExt
);

kind!(
    /// XPUB: PUB that also delivers subscription messages to the user.
    XPub, SocketType::Xpub, XPubHooks, PubExt
);

/// Scratch space a publisher reuses for each message's matching peers.
#[derive(Default)]
pub struct PubExt {
    keys: SmallVec<[u64; 16]>,
    ids: Vec<PeerId>,
}

trait PubExtAccess {
    fn ext_pub(&mut self) -> &mut PubExt;
}

impl PubExtAccess for Socket<Pub> {
    fn ext_pub(&mut self) -> &mut PubExt {
        &mut self.ext
    }
}

impl PubExtAccess for Socket<XPub> {
    fn ext_pub(&mut self) -> &mut PubExt {
        &mut self.ext
    }
}

impl Socket<Pub> {
    /// Publish `msg` to every matching subscriber. Subscribers at their HWM
    /// miss it; publishing never waits. Returns how many got it.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<usize> {
        let msg = msg.into();
        Ok(publish_split(self, &msg, |h| &h.index))
    }

    /// Distinct prefixes the subscriber `peer` holds.
    #[must_use]
    pub fn subscriber_prefixes(&self, peer: PeerId) -> usize {
        self.core.borrow().hooks().prefixes_of(peer)
    }
}

impl Socket<XPub> {
    /// Publish `msg` to every matching subscriber. See [`Socket::<Pub>::send`].
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<usize> {
        let msg = msg.into();
        Ok(publish_split(self, &msg, |h| &h.0.index))
    }

    /// Receive the next subscription (`\x01topic`) or unsubscription
    /// (`\x00topic`) message.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_fair().await
    }

    /// Receive and keep the next subscription message.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_fair().await.map(MsgRef::take)
    }

    /// Receive the next subscription change, decoded.
    pub async fn recv_subscription(&mut self) -> Result<SubscriptionEvent> {
        loop {
            let msg = self.recv_owned().await?;
            if let Some(ev) = msg.first().and_then(SubscriptionEvent::from_message) {
                return Ok(ev);
            }
        }
    }
}

/// Match and fan out without holding the core borrow and the scratch borrow
/// at once.
fn publish_split<K: Kind>(
    socket: &mut Socket<K>,
    msg: &Msg,
    index: impl Fn(&K::Hooks) -> &SubscriptionIndex,
) -> usize
where
    Socket<K>: PubExtAccess,
{
    let topic = msg.first().unwrap_or_default();
    let mut ids = std::mem::take(&mut socket.ext_pub().ids);
    let mut keys = std::mem::take(&mut socket.ext_pub().keys);
    ids.clear();
    let sent = {
        let mut core = socket.core.borrow_mut();
        core.progress();
        let invert = core.options().invert_matching;
        let idx = index(core.hooks());
        if invert {
            ids.extend(
                core.peer_ids()
                    .iter()
                    .copied()
                    .filter(|id| !idx.peer_matches(id.as_u64(), topic)),
            );
        } else {
            idx.match_peers(topic, &mut keys);
            ids.extend(keys.iter().map(|&k| PeerId::from_u64(k)));
        }
        core.fanout_to(msg, &ids)
    };
    let ext = socket.ext_pub();
    ext.ids = ids;
    ext.keys = keys;
    sent
}

// ── SUB / XSUB ───────────────────────────────────────────────────────────

/// A subscriber's local subscriptions, used to filter what it receives.
pub struct SubHooks {
    subs: SubscriptionTrie,
    invert: bool,
    filter: bool,
}

impl FromOptions for SubHooks {
    fn from_options(options: &SocketOptions) -> Self {
        Self {
            subs: SubscriptionTrie::new(),
            invert: options.invert_matching,
            filter: true,
        }
    }
}

/// XSUB does not filter: it receives whatever its publishers sent.
pub struct XSubHooks(SubHooks);

impl FromOptions for XSubHooks {
    fn from_options(options: &SocketOptions) -> Self {
        let mut h = SubHooks::from_options(options);
        h.filter = false;
        Self(h)
    }
}

impl Hooks for SubHooks {
    fn greeting(&self) -> Vec<Msg> {
        subscription_greeting(&self.subs)
    }

    fn on_inbound(&mut self, _id: PeerId, msg: Msg) -> Option<Msg> {
        if !self.filter {
            return Some(msg);
        }
        let topic = msg.first().unwrap_or_default();
        (self.subs.matches(topic) != self.invert).then_some(msg)
    }
}

impl Hooks for XSubHooks {
    fn greeting(&self) -> Vec<Msg> {
        subscription_greeting(&self.0.subs)
    }

    fn on_inbound(&mut self, id: PeerId, msg: Msg) -> Option<Msg> {
        self.0.on_inbound(id, msg)
    }
}

/// SUB: receives the messages matching its subscriptions; cannot send.
pub struct Sub;

impl Kind for Sub {
    const TYPE: SocketType = SocketType::Sub;
    type Hooks = SubHooks;
    type Ext = ();

    fn hooks(options: &SocketOptions) -> SubHooks {
        SubHooks::from_options(options)
    }

    fn init(socket: &mut Socket<Self>) {
        for prefix in socket.options().subscriptions {
            change_subscription(socket, |h| &mut h.subs, true, &prefix);
        }
    }
}

kind!(
    /// XSUB: sends subscription messages upstream and receives everything.
    XSub, SocketType::Xsub, XSubHooks, ()
);

/// Subscription bookkeeping shared by SUB and XSUB.
fn change_subscription<K: Kind>(
    socket: &mut Socket<K>,
    subs: impl Fn(&mut K::Hooks) -> &mut SubscriptionTrie,
    subscribe: bool,
    prefix: &[u8],
) {
    let mut core = socket.core.borrow_mut();
    let changed = {
        let t = subs(core.hooks_mut());
        if subscribe {
            t.subscribe(Bytes::copy_from_slice(prefix))
        } else {
            t.unsubscribe(prefix)
        }
    };
    if !changed {
        return;
    }
    // Peers that connect later get the full set from `Hooks::greeting`; the
    // ones connected now get the change, never dropped at the HWM.
    core.broadcast_control(&subscription_message(subscribe, prefix));
}

/// `\x01prefix` to subscribe, `\x00prefix` to cancel.
fn subscription_message(subscribe: bool, prefix: &[u8]) -> Msg {
    let mut body = Vec::with_capacity(1 + prefix.len());
    body.push(u8::from(subscribe));
    body.extend_from_slice(prefix);
    Msg::from(body)
}

fn subscription_greeting(subs: &SubscriptionTrie) -> Vec<Msg> {
    subs.subscriptions()
        .iter()
        .map(|s| subscription_message(true, &s.prefix))
        .collect()
}

impl Socket<Sub> {
    /// Subscribe to messages whose first frame starts with `prefix` (empty for
    /// everything).
    pub async fn subscribe(&mut self, prefix: impl AsRef<[u8]>) -> Result<()> {
        change_subscription(self, |h| &mut h.subs, true, prefix.as_ref());
        Ok(())
    }

    /// Drop one subscription to `prefix`.
    pub async fn unsubscribe(&mut self, prefix: impl AsRef<[u8]>) -> Result<()> {
        change_subscription(self, |h| &mut h.subs, false, prefix.as_ref());
        Ok(())
    }

    /// Receive the next matching message.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_fair().await
    }

    /// Receive and keep the next matching message.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_fair().await.map(MsgRef::take)
    }

    /// The current subscriptions.
    #[must_use]
    pub fn subscriptions(&self) -> Vec<Bytes> {
        self.core
            .borrow()
            .hooks()
            .subs
            .subscriptions()
            .into_iter()
            .map(|s| s.prefix)
            .collect()
    }
}

impl Socket<XSub> {
    /// Send upstream. A message whose first frame starts with `1` or `0` is a
    /// subscription or cancellation and is also remembered, so it is replayed
    /// to publishers that connect later; anything else goes to every
    /// publisher as is.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        let msg = msg.into();
        let first = msg.first().unwrap_or_default();
        match first.split_first() {
            Some((&k @ (0 | 1), prefix)) if msg.len() == 1 => {
                let prefix = prefix.to_vec();
                change_subscription(self, |h| &mut h.0.subs, k == 1, &prefix);
            }
            _ => {
                self.core.borrow_mut().fanout(&msg, |_, _| true);
            }
        }
        Ok(())
    }

    /// Subscribe upstream to `prefix`.
    pub async fn subscribe(&mut self, prefix: impl AsRef<[u8]>) -> Result<()> {
        change_subscription(self, |h| &mut h.0.subs, true, prefix.as_ref());
        Ok(())
    }

    /// Cancel one upstream subscription to `prefix`.
    pub async fn unsubscribe(&mut self, prefix: impl AsRef<[u8]>) -> Result<()> {
        change_subscription(self, |h| &mut h.0.subs, false, prefix.as_ref());
        Ok(())
    }

    /// Receive the next message from any publisher.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_fair().await
    }

    /// Receive and keep the next message.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_fair().await.map(MsgRef::take)
    }
}

/// Re-exported socket aliases.
pub type PairSocket = Socket<Pair>;
/// DEALER socket.
pub type DealerSocket = Socket<Dealer>;
/// ROUTER socket.
pub type RouterSocket = Socket<Router>;
/// REQ socket.
pub type ReqSocket = Socket<Req>;
/// REP socket.
pub type RepSocket = Socket<Rep>;
/// PUSH socket.
pub type PushSocket = Socket<Push>;
/// PULL socket.
pub type PullSocket = Socket<Pull>;
/// PUB socket.
pub type PubSocket = Socket<Pub>;
/// SUB socket.
pub type SubSocket = Socket<Sub>;
/// XPUB socket.
pub type XPubSocket = Socket<XPub>;
/// XSUB socket.
pub type XSubSocket = Socket<XSub>;
