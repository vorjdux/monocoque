//! PUB/SUB Hub (Phase 3)
//!
//! Responsibilities:
//! - Maintain a stable mapping from RoutingID -> PeerKey (compact u64).
//! - Track active peers with an Epoch to avoid ghost-peer races.
//! - Apply SUB / UNSUB commands to the SubscriptionIndex.
//! - Fan out published messages to matching peers (zero-copy via Bytes).
//!
//! Concurrency model:
//! - Single-threaded async task.
//! - Uses flume::select! for runtime-agnostic multiplexing.
//! - No locks on the hot publish path.

use crate::pubsub::index::{PeerKey, SubscriptionIndex};
use crate::router::PeerCmd;

use bytes::Bytes;
use flume::{Receiver, Sender};
use hashbrown::HashMap;

/// Commands from application to PubSub Hub
#[derive(Debug)]
pub enum PubSubCmd {
    /// Publish a message (frame 0 is topic)
    Publish(Vec<Bytes>),
    /// Close all peers
    Close,
}

/// Events coming from peer actors (SUB sockets).
///
/// These are emitted by SocketActor when:
/// - handshake completes
/// - connection closes
/// - SUB / UNSUB commands are parsed
#[derive(Debug)]
pub enum PubSubEvent {
    PeerUp {
        routing_id: Bytes,
        epoch: u64,
        tx: Sender<PeerCmd>,
    },
    PeerDown {
        routing_id: Bytes,
        epoch: u64,
    },
    Subscribe {
        routing_id: Bytes,
        prefix: Bytes,
    },
    Unsubscribe {
        routing_id: Bytes,
        prefix: Bytes,
    },
}

/// Supervisor for PUB/SUB sockets.
///
/// This hub does *no* I/O itself.
/// It only routes already-decoded messages between actors.
pub struct PubSubHub {
    /// Subscription index (topic -> peers)
    index: SubscriptionIndex,

    /// Stable mapping: RoutingID -> PeerKey
    rid_to_key: HashMap<Bytes, PeerKey>,

    /// Reverse mapping for cleanup/debug
    key_to_rid: HashMap<PeerKey, Bytes>,

    /// Active peers: PeerKey -> (epoch, sender)
    peers: HashMap<PeerKey, (u64, Sender<PeerCmd>)>,

    /// Monotonic key generator
    next_key: PeerKey,

    /// Events from actors
    hub_rx: Receiver<PubSubEvent>,

    /// Messages from user (publish path)
    user_tx_rx: Receiver<PubSubCmd>,
}

impl PubSubHub {
    pub fn new(hub_rx: Receiver<PubSubEvent>, user_tx_rx: Receiver<PubSubCmd>) -> Self {
        Self {
            index: SubscriptionIndex::new(),
            rid_to_key: HashMap::new(),
            key_to_rid: HashMap::new(),
            peers: HashMap::new(),
            next_key: 1, // reserve 0
            hub_rx,
            user_tx_rx,
        }
    }

    /// Main event loop.
    pub async fn run(mut self) {
        use futures::select;
        use futures::FutureExt;
        
        loop {
            // Use futures::select! for runtime-agnostic multiplexing
            select! {
                msg = self.hub_rx.recv_async().fuse() => {
                    match msg {
                        Ok(ev) => self.on_hub_event(ev),
                        Err(_) => break, // shutdown
                    }
                }
                msg = self.user_tx_rx.recv_async().fuse() => {
                    match msg {
                        Ok(cmd) => self.on_user_cmd(cmd),
                        Err(_) => break, // shutdown
                    }
                }
            }
        }
    }

    fn on_hub_event(&mut self, ev: PubSubEvent) {
        match ev {
            PubSubEvent::PeerUp {
                routing_id,
                epoch,
                tx,
            } => {
                // Resolve or allocate PeerKey
                let key = if let Some(&k) = self.rid_to_key.get(&routing_id) {
                    k
                } else {
                    let k = self.next_key;
                    self.next_key += 1;
                    self.rid_to_key.insert(routing_id.clone(), k);
                    self.key_to_rid.insert(k, routing_id.clone());
                    k
                };

                // Overwrite any previous epoch (reconnect case)
                self.peers.insert(key, (epoch, tx));
            }

            PubSubEvent::PeerDown { routing_id, epoch } => {
                if let Some(&key) = self.rid_to_key.get(&routing_id) {
                    if let Some((current_epoch, _)) = self.peers.get(&key) {
                        // Epoch check prevents ghost-peer removal
                        if *current_epoch == epoch {
                            self.peers.remove(&key);
                            self.index.remove_peer_everywhere(key);
                        }
                    }
                }
            }

            PubSubEvent::Subscribe { routing_id, prefix } => {
                if let Some(&key) = self.rid_to_key.get(&routing_id) {
                    if self.peers.contains_key(&key) {
                        self.index.subscribe(key, prefix);
                    }
                }
            }

            PubSubEvent::Unsubscribe { routing_id, prefix } => {
                if let Some(&key) = self.rid_to_key.get(&routing_id) {
                    self.index.unsubscribe(key, &prefix);
                }
            }
        }
    }

    fn on_user_cmd(&mut self, cmd: PubSubCmd) {
        match cmd {
            PubSubCmd::Publish(parts) => self.publish(parts),
            PubSubCmd::Close => {
                // Broadcast close to all peers
                for (_, (_, tx)) in self.peers.iter() {
                    let _ = tx.send(PeerCmd::Close);
                }
            }
        }
    }

    /// Publish a multipart message.
    ///
    /// ZMQ convention:
    /// - Frame 0 is the topic
    fn publish(&mut self, parts: Vec<Bytes>) {
        if parts.is_empty() || self.index.is_empty() {
            return;
        }

        let topic = &parts[0];
        let keys = self.index.match_topic(topic);

        if keys.is_empty() {
            return;
        }

        // Zero-copy fanout:
        // - Vec<Bytes> is cloned (cheap)
        // - Bytes are refcounted
        for key in keys {
            if let Some((_, tx)) = self.peers.get(&key) {
                let _ = tx.send(PeerCmd::SendBody(parts.clone()));
            }
        }
    }
}
