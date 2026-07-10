//! PUB/SUB Hub (Phase 3)
//!
//! Responsibilities:
//! - Maintain a stable mapping from `RoutingID` -> `PeerKey` (compact u64).
//! - Track active peers with an Epoch to avoid ghost-peer races.
//! - Apply SUB / UNSUB commands to the `SubscriptionIndex`.
//! - Fan out published messages to matching peers (zero-copy via Bytes).
//!
//! Concurrency model:
//! - Single-threaded async task.
//! - Uses `flume::select`! for runtime-agnostic multiplexing.
//! - No locks on the hot publish path.

use crate::pubsub::index::{PeerKey, SubscriptionIndex};
use crate::router::PeerCmd;

use bytes::Bytes;
use flume::{Receiver, Sender};
use hashbrown::HashMap;
use std::collections::HashMap as StdHashMap;
use std::collections::hash_map::RandomState;
use std::sync::Arc;

/// Map keyed by peer-reported routing identity.
///
/// The routing id is attacker-controlled, so this uses the per-process
/// randomly seeded [`RandomState`] (`SipHash`) to resist hash-flooding. The
/// `PeerKey`-keyed maps below stay on the faster default hasher because their
/// keys are server-assigned monotonic counters, not attacker input.
type RidMap<V> = StdHashMap<Bytes, V, RandomState>;

/// Commands from application to `PubSub` Hub
#[derive(Debug)]
pub enum PubSubCmd {
    /// Publish a message (frame 0 is topic)
    Publish(Vec<Bytes>),
    /// Close all peers
    Close,
}

/// Events coming from peers (SUB sockets).
///
/// These are emitted when:
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
/// It only routes already-decoded messages between peers.
pub struct PubSubHub {
    /// Subscription index (topic -> peers)
    index: SubscriptionIndex,

    /// Stable mapping: `RoutingID` -> `PeerKey` (attacker-keyed; seeded hasher)
    rid_to_key: RidMap<PeerKey>,

    /// Reverse mapping for cleanup/debug
    key_to_rid: HashMap<PeerKey, Bytes>,

    /// Active peers: `PeerKey` -> (epoch, sender)
    peers: HashMap<PeerKey, (u64, Sender<PeerCmd>)>,

    /// Monotonic key generator
    next_key: PeerKey,

    /// Events from peers
    hub_rx: Receiver<PubSubEvent>,

    /// Messages from user (publish path)
    user_tx_rx: Receiver<PubSubCmd>,
}

impl PubSubHub {
    #[must_use]
    pub fn new(hub_rx: Receiver<PubSubEvent>, user_tx_rx: Receiver<PubSubCmd>) -> Self {
        Self {
            index: SubscriptionIndex::new(),
            rid_to_key: RidMap::default(),
            key_to_rid: HashMap::new(),
            peers: HashMap::new(),
            next_key: 1, // reserve 0
            hub_rx,
            user_tx_rx,
        }
    }

    /// Main event loop.
    pub async fn run(mut self) {
        use futures::FutureExt;
        use futures::select;

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
                    // Single clone for both bidirectional map inserts
                    self.key_to_rid.insert(k, routing_id.clone());
                    self.rid_to_key.insert(routing_id, k);
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
                for (_, (_, tx)) in &self.peers {
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

        // Zero-copy fan-out: share one allocation across all matching peers via
        // Arc instead of cloning a fresh Vec<Bytes> per peer. Each peer gets an
        // Arc refcount bump; the frames themselves are never re-copied.
        let msg = Arc::new(parts);
        for key in keys {
            if let Some((_, tx)) = self.peers.get(&key) {
                let _ = tx.send(PeerCmd::SendBody(Arc::clone(&msg)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn b(s: &str) -> Bytes {
        Bytes::copy_from_slice(s.as_bytes())
    }

    /// Receive the next `PeerCmd` body Arc within a timeout; None on timeout/close.
    async fn recv_arc(rx: &Receiver<PeerCmd>) -> Option<Arc<Vec<Bytes>>> {
        match crate::rt::timeout(Duration::from_secs(1), rx.recv_async()).await {
            Ok(Ok(PeerCmd::SendBody(parts))) => Some(parts),
            _ => None,
        }
    }

    /// True if no message body arrives within a short window.
    async fn expect_no_body(rx: &Receiver<PeerCmd>) -> bool {
        !matches!(
            crate::rt::timeout(Duration::from_millis(150), rx.recv_async()).await,
            Ok(Ok(PeerCmd::SendBody(_)))
        )
    }

    #[test]
    fn publishes_only_to_matching_subscribers() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (hub_tx, hub_rx) = flume::unbounded::<PubSubEvent>();
            let (user_tx, user_rx) = flume::unbounded::<PubSubCmd>();
            let hub = PubSubHub::new(hub_rx, user_rx);
            let handle = crate::rt::spawn(hub.run());

            let (peer_tx, peer_rx) = flume::unbounded::<PeerCmd>();
            hub_tx
                .send(PubSubEvent::PeerUp {
                    routing_id: b("sub1"),
                    epoch: 1,
                    tx: peer_tx,
                })
                .unwrap();
            hub_tx
                .send(PubSubEvent::Subscribe {
                    routing_id: b("sub1"),
                    prefix: b("weather."),
                })
                .unwrap();
            crate::rt::sleep(Duration::from_millis(30)).await;

            // Matching topic is delivered.
            user_tx
                .send(PubSubCmd::Publish(vec![b("weather.london"), b("sunny")]))
                .unwrap();
            let got = recv_arc(&peer_rx).await.expect("matching topic delivered");
            assert_eq!(*got, vec![b("weather.london"), b("sunny")]);

            // Non-matching topic is filtered out.
            user_tx
                .send(PubSubCmd::Publish(vec![b("stocks.aapl"), b("100")]))
                .unwrap();
            assert!(
                expect_no_body(&peer_rx).await,
                "non-matching topic must not be delivered"
            );

            drop(hub_tx);
            drop(user_tx);
            crate::rt::join(handle).await;
        });
    }

    #[test]
    fn fanout_shares_one_allocation_across_peers() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (hub_tx, hub_rx) = flume::unbounded::<PubSubEvent>();
            let (user_tx, user_rx) = flume::unbounded::<PubSubCmd>();
            let hub = PubSubHub::new(hub_rx, user_rx);
            let handle = crate::rt::spawn(hub.run());

            let (p1_tx, p1_rx) = flume::unbounded::<PeerCmd>();
            let (p2_tx, p2_rx) = flume::unbounded::<PeerCmd>();
            for (rid, tx) in [(b("s1"), p1_tx), (b("s2"), p2_tx)] {
                hub_tx
                    .send(PubSubEvent::PeerUp {
                        routing_id: rid.clone(),
                        epoch: 1,
                        tx,
                    })
                    .unwrap();
                hub_tx
                    .send(PubSubEvent::Subscribe {
                        routing_id: rid,
                        prefix: b(""), // subscribe to all
                    })
                    .unwrap();
            }
            crate::rt::sleep(Duration::from_millis(30)).await;

            user_tx
                .send(PubSubCmd::Publish(vec![b("news"), b("hello")]))
                .unwrap();

            let a = recv_arc(&p1_rx).await.expect("peer 1 delivered");
            let c = recv_arc(&p2_rx).await.expect("peer 2 delivered");
            assert_eq!(*a, vec![b("news"), b("hello")]);
            assert_eq!(*c, vec![b("news"), b("hello")]);
            // Both peers received the SAME allocation: fan-out shared one Arc
            // instead of cloning a fresh Vec<Bytes> per peer.
            assert!(
                Arc::ptr_eq(&a, &c),
                "fan-out must share one Arc allocation across peers"
            );

            drop(hub_tx);
            drop(user_tx);
            crate::rt::join(handle).await;
        });
    }

    #[test]
    fn peer_down_with_stale_epoch_is_ignored() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (hub_tx, hub_rx) = flume::unbounded::<PubSubEvent>();
            let (user_tx, user_rx) = flume::unbounded::<PubSubCmd>();
            let hub = PubSubHub::new(hub_rx, user_rx);
            let handle = crate::rt::spawn(hub.run());

            let (peer_tx, peer_rx) = flume::unbounded::<PeerCmd>();
            hub_tx
                .send(PubSubEvent::PeerUp {
                    routing_id: b("sub1"),
                    epoch: 2,
                    tx: peer_tx,
                })
                .unwrap();
            hub_tx
                .send(PubSubEvent::Subscribe {
                    routing_id: b("sub1"),
                    prefix: b(""),
                })
                .unwrap();
            // A PeerDown carrying a stale epoch must NOT evict the live peer.
            hub_tx
                .send(PubSubEvent::PeerDown {
                    routing_id: b("sub1"),
                    epoch: 1,
                })
                .unwrap();
            crate::rt::sleep(Duration::from_millis(30)).await;

            user_tx
                .send(PubSubCmd::Publish(vec![b("x"), b("still-here")]))
                .unwrap();
            let got = recv_arc(&peer_rx)
                .await
                .expect("stale-epoch PeerDown must not evict the peer");
            assert_eq!(*got, vec![b("x"), b("still-here")]);

            drop(hub_tx);
            drop(user_tx);
            crate::rt::join(handle).await;
        });
    }
}
