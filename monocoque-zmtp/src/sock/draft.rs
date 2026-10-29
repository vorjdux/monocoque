//! The draft socket types, behind the `draft` feature: CLIENT/SERVER,
//! RADIO/DISH, SCATTER/GATHER and CHANNEL.
//!
//! All of them carry single-part messages. A send of more than one frame
//! fails with `InvalidState`, and a multipart message from a peer is dropped.
//! They need ZMTP 3.1, so a ZMTP 1.0 or 2.0 peer is refused at the greeting.
//!
//! SERVER addresses peers by a nonzero 32-bit routing id it assigns, handed
//! out with each received message. RADIO publishes to groups of at most 255
//! bytes; DISH joins groups exactly (no prefix matching) and receives
//! `[group, body]`. On the wire DISH sends JOIN/LEAVE commands and RADIO
//! sends the group as its own frame ahead of the body, as libzmq does.

use super::{Core, FromOptions, PairHooks};
use crate::session::SocketType;
use crate::sock::{Kind, Socket};
use bytes::Bytes;
use monocoque_core::error::{MonocoqueError, Result};
use monocoque_core::msg::{Msg, MsgRef};
use monocoque_core::mux::{Hooks, PeerId, PeerInfo, SendTo};
use monocoque_core::options::SocketOptions;
use std::collections::{HashMap, HashSet};

/// Longest RADIO/DISH group name, as in libzmq.
pub const MAX_GROUP: usize = 255;

fn single(msg: Msg) -> Result<Msg> {
    if msg.len() == 1 {
        Ok(msg)
    } else {
        Err(MonocoqueError::InvalidState(
            "draft socket types send single-part messages",
        ))
    }
}

fn check_group(group: &[u8]) -> Result<()> {
    if group.len() > MAX_GROUP {
        return Err(MonocoqueError::InvalidState(
            "group names are at most 255 bytes",
        ));
    }
    Ok(())
}

/// Hooks that drop multipart messages from peers.
#[derive(Default)]
pub struct SinglePart;

impl FromOptions for SinglePart {
    fn from_options(_: &SocketOptions) -> Self {
        Self
    }
}

impl Hooks for SinglePart {
    fn on_inbound(&mut self, _id: PeerId, msg: Msg) -> Option<Msg> {
        (msg.len() == 1).then_some(msg)
    }
}

impl<K: Kind> Socket<K> {
    async fn recv_single(&mut self) -> Result<MsgRef<'_>> {
        self.next_into_slot().await?;
        Ok(self.lend())
    }
}

// ── CLIENT / SCATTER / GATHER ─────────────────────────────────────────────

kind!(
    /// CLIENT: round-robin sends to SERVER peers, fair-queued receives.
    Client, SocketType::Client, SinglePart, ()
);

impl Socket<Client> {
    /// Send to the next SERVER with room, waiting while none has.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        self.send_lb(single(msg.into())?).await.map(drop)
    }

    /// Receive the next message from any peer.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_single().await
    }

    /// Receive the next message and keep it.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_single().await.map(MsgRef::take)
    }
}

kind!(
    /// SCATTER: round-robin sends to GATHER peers; cannot receive.
    Scatter, SocketType::Scatter, SinglePart, (), {
        fn readable((): &(), _: &mut Core<SinglePart>) -> bool { false }
    }
);

impl Socket<Scatter> {
    /// Send to the next peer with room, waiting while none has.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        self.send_lb(single(msg.into())?).await.map(drop)
    }
}

kind!(
    /// GATHER: fair-queued receives from SCATTER peers; cannot send.
    Gather, SocketType::Gather, SinglePart, (), {
        fn writable((): &(), _: &mut Core<SinglePart>) -> bool { false }
    }
);

impl Socket<Gather> {
    /// Receive the next message from any peer.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_single().await
    }

    /// Receive the next message and keep it.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_single().await.map(MsgRef::take)
    }
}

// ── CHANNEL ───────────────────────────────────────────────────────────────

/// CHANNEL: PAIR's one exclusive peer, single-part.
#[derive(Default)]
pub struct ChannelHooks(PairHooks);

impl FromOptions for ChannelHooks {
    fn from_options(_: &SocketOptions) -> Self {
        Self::default()
    }
}

impl Hooks for ChannelHooks {
    fn on_peer_up(&mut self, id: PeerId, info: &PeerInfo) -> bool {
        self.0.on_peer_up(id, info)
    }

    fn on_peer_down(&mut self, id: PeerId, info: &PeerInfo) {
        self.0.on_peer_down(id, info);
    }

    fn on_inbound(&mut self, _id: PeerId, msg: Msg) -> Option<Msg> {
        (msg.len() == 1).then_some(msg)
    }
}

kind!(
    /// CHANNEL: one exclusive bidirectional peer, single-part messages.
    Channel, SocketType::Channel, ChannelHooks, ()
);

impl Socket<Channel> {
    /// Send to the peer, waiting for one if none is connected.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        self.send_lb(single(msg.into())?).await.map(drop)
    }

    /// Receive the next message.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_single().await
    }

    /// Receive the next message and keep it.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_single().await.map(MsgRef::take)
    }
}

// ── SERVER ────────────────────────────────────────────────────────────────

/// SERVER's routing ids. A departed peer keeps its id until its slot is
/// reused, so messages it sent before leaving still arrive addressed.
pub struct ServerHooks {
    by_id: HashMap<u32, PeerId>,
    id_of: HashMap<PeerId, u32>,
    departed: HashMap<u32, (PeerId, u32)>,
    next: u32,
}

impl FromOptions for ServerHooks {
    fn from_options(_: &SocketOptions) -> Self {
        Self {
            by_id: HashMap::new(),
            id_of: HashMap::new(),
            departed: HashMap::new(),
            next: rand::random(),
        }
    }
}

impl ServerHooks {
    fn routing_id_of(&self, peer: PeerId) -> Option<u32> {
        self.id_of.get(&peer).copied().or_else(|| {
            self.departed
                .get(&peer.index())
                .filter(|(p, _)| *p == peer)
                .map(|&(_, rid)| rid)
        })
    }
}

impl Hooks for ServerHooks {
    fn on_peer_up(&mut self, id: PeerId, _info: &PeerInfo) -> bool {
        let rid = loop {
            self.next = self.next.wrapping_add(1);
            if self.next != 0 && !self.by_id.contains_key(&self.next) {
                break self.next;
            }
        };
        self.departed.remove(&id.index());
        self.by_id.insert(rid, id);
        self.id_of.insert(id, rid);
        true
    }

    fn on_peer_down(&mut self, id: PeerId, _info: &PeerInfo) {
        if let Some(rid) = self.id_of.remove(&id) {
            self.by_id.remove(&rid);
            self.departed.insert(id.index(), (id, rid));
        }
    }

    fn on_inbound(&mut self, _id: PeerId, msg: Msg) -> Option<Msg> {
        (msg.len() == 1).then_some(msg)
    }
}

kind!(
    /// SERVER: receives carry the sender's routing id; sends go to one.
    Server, SocketType::Server, ServerHooks, (), {
        // A send never waits: it fails with `Again` at the peer's HWM.
        fn writable((): &(), _: &mut Core<ServerHooks>) -> bool { true }
    }
);

impl Socket<Server> {
    /// Receive the next message and its sender's routing id.
    pub async fn recv(&mut self) -> Result<(u32, MsgRef<'_>)> {
        loop {
            let peer = self.next_into_slot().await?;
            let rid = self.core.borrow().hooks().routing_id_of(peer);
            if let Some(rid) = rid {
                return Ok((rid, self.lend()));
            }
        }
    }

    /// Receive and keep the next message and its sender's routing id.
    pub async fn recv_owned(&mut self) -> Result<(u32, Msg)> {
        let (rid, msg) = self.recv().await?;
        Ok((rid, msg.take()))
    }

    /// Send `msg` to the peer `routing_id`. Fails with `HostUnreachable` if
    /// no such peer is connected and `Again` if it is at its HWM.
    pub async fn send(&mut self, routing_id: u32, msg: impl Into<Msg>) -> Result<()> {
        let msg = single(msg.into())?;
        let peer = self.core.borrow().hooks().by_id.get(&routing_id).copied();
        let Some(peer) = peer else {
            return Err(MonocoqueError::HostUnreachable);
        };
        match self.send_to_now(peer, msg) {
            SendTo::Sent => {
                self.wait_written(peer).await;
                Ok(())
            }
            SendTo::Full(_) => Err(MonocoqueError::Again),
            SendTo::Gone(_) => Err(MonocoqueError::HostUnreachable),
        }
    }
}

// ── RADIO / DISH ──────────────────────────────────────────────────────────

/// RADIO's group membership, fed by its DISH peers' JOIN and LEAVE.
#[derive(Default)]
pub struct RadioHooks {
    members: HashMap<Bytes, Vec<PeerId>>,
}

impl FromOptions for RadioHooks {
    fn from_options(_: &SocketOptions) -> Self {
        Self::default()
    }
}

impl Hooks for RadioHooks {
    fn eager_inbound(&self) -> bool {
        true
    }

    fn on_inbound(&mut self, id: PeerId, msg: Msg) -> Option<Msg> {
        let frame = msg.frame_bytes(0)?;
        let (&kind, _) = frame.split_first()?;
        let group = frame.slice(1..);
        if kind == 1 {
            let peers = self.members.entry(group).or_default();
            if !peers.contains(&id) {
                peers.push(id);
            }
        } else if let Some(peers) = self.members.get_mut(&group) {
            peers.retain(|&p| p != id);
            if peers.is_empty() {
                self.members.remove(&group);
            }
        }
        None
    }

    fn on_peer_down(&mut self, id: PeerId, _info: &PeerInfo) {
        self.members.retain(|_, peers| {
            peers.retain(|&p| p != id);
            !peers.is_empty()
        });
    }
}

kind!(
    /// RADIO: each message goes to the DISH peers that joined its group;
    /// cannot receive.
    Radio, SocketType::Radio, RadioHooks, RadioExt, {
        fn readable(_: &RadioExt, _: &mut Core<RadioHooks>) -> bool { false }
        // Like PUB, a send never waits: a peer at its HWM misses it.
        fn writable(_: &RadioExt, _: &mut Core<RadioHooks>) -> bool { true }
    }
);

/// Scratch a RADIO reuses for each message's recipients.
#[derive(Default)]
pub struct RadioExt {
    ids: Vec<PeerId>,
}

impl Socket<Radio> {
    /// Publish `msg` to the members of `group`. Peers at their HWM miss it;
    /// a send never waits. Returns how many peers got it.
    pub async fn send(&mut self, group: impl Into<Bytes>, msg: impl Into<Msg>) -> Result<usize> {
        let group = group.into();
        check_group(&group)?;
        let msg = single(msg.into())?;
        let body = msg.frame_bytes(0).unwrap_or_default();
        let wire = Msg::from([group, body]);
        let mut ids = std::mem::take(&mut self.ext.ids);
        ids.clear();
        let sent = {
            let mut core = self.core.borrow_mut();
            core.progress();
            if let Some(peers) = core.hooks().members.get(wire.first().unwrap_or_default()) {
                ids.extend_from_slice(peers);
            }
            core.fanout_to(&wire, &ids)
        };
        self.ext.ids = ids;
        Ok(sent)
    }
}

/// DISH's joined groups, used to filter what it receives.
#[derive(Default)]
pub struct DishHooks {
    groups: HashSet<Bytes>,
}

impl FromOptions for DishHooks {
    fn from_options(_: &SocketOptions) -> Self {
        Self::default()
    }
}

fn group_message(join: bool, group: &[u8]) -> Msg {
    let mut body = Vec::with_capacity(1 + group.len());
    body.push(u8::from(join));
    body.extend_from_slice(group);
    Msg::from(body)
}

impl Hooks for DishHooks {
    fn greeting(&self) -> Vec<Msg> {
        self.groups.iter().map(|g| group_message(true, g)).collect()
    }

    fn on_inbound(&mut self, _id: PeerId, msg: Msg) -> Option<Msg> {
        (msg.len() == 2 && self.groups.contains(msg.first()?)).then_some(msg)
    }
}

kind!(
    /// DISH: receives `[group, body]` for the groups it joined; cannot send.
    Dish, SocketType::Dish, DishHooks, (), {
        fn writable((): &(), _: &mut Core<DishHooks>) -> bool { false }
    }
);

impl Socket<Dish> {
    /// Join `group`, an exact name of at most 255 bytes.
    pub async fn join(&mut self, group: impl AsRef<[u8]>) -> Result<()> {
        let group = group.as_ref();
        check_group(group)?;
        let mut core = self.core.borrow_mut();
        if core
            .hooks_mut()
            .groups
            .insert(Bytes::copy_from_slice(group))
        {
            core.broadcast_control(&group_message(true, group));
        }
        Ok(())
    }

    /// Leave `group`.
    pub async fn leave(&mut self, group: impl AsRef<[u8]>) -> Result<()> {
        let group = group.as_ref();
        let mut core = self.core.borrow_mut();
        if core.hooks_mut().groups.remove(group) {
            core.broadcast_control(&group_message(false, group));
        }
        Ok(())
    }

    /// Receive the next `[group, body]` message for a joined group.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        self.recv_single().await
    }

    /// Receive and keep the next `[group, body]` message.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv_single().await.map(MsgRef::take)
    }
}

/// CLIENT socket.
pub type ClientSocket = Socket<Client>;
/// SERVER socket.
pub type ServerSocket = Socket<Server>;
/// RADIO socket.
pub type RadioSocket = Socket<Radio>;
/// DISH socket.
pub type DishSocket = Socket<Dish>;
/// SCATTER socket.
pub type ScatterSocket = Socket<Scatter>;
/// GATHER socket.
pub type GatherSocket = Socket<Gather>;
/// CHANNEL socket.
pub type ChannelSocket = Socket<Channel>;
