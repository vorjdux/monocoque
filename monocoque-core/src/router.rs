//! ROUTER Hub (Phase 2.1)
//!
//! Goals:
//! - Runtime-agnostic async loop (`flume::select`!, no tokio)
//! - Strict types: `RouterCmd` has envelope, `PeerCmd` is body-only
//! - Envelope normalization:
//!   - inbound (actor->user) is normalized elsewhere to [ID, Empty, Body...]
//!   - outbound (user->hub) accepts [ID, (Empty), Body...] in Standard mode
//! - Load balancer mode: round-robin dispatch when no explicit routing id is used
//! - "Ghost peer" self-heal: stale IDs removed from rr list when detected

use bytes::Bytes;
use flume::{Receiver, Sender};
use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::sync::Arc;

/// Map keyed by peer-reported routing identity.
///
/// The key is attacker-controlled (a peer announces its own routing id), so
/// this uses the per-process randomly seeded [`RandomState`] (SipHash) rather
/// than a fixed-seed hasher, to resist hash-flooding.
type PeerMap<V> = HashMap<Bytes, V, RandomState>;

/// Commands sent from application to Router Hub
#[derive(Debug)]
pub enum RouterCmd {
    /// Send a message (with routing envelope in Standard mode, or body-only in LB mode)
    SendMessage(Vec<Bytes>),
    /// Close all peers
    Close,
}

/// Commands sent from Hub -> Peer (body only; hub strips any envelope)
#[derive(Debug)]
pub enum PeerCmd {
    /// Send a message body to the peer.
    ///
    /// The frames are shared via `Arc` so a fan-out (PUB/SUB) hands every
    /// matching peer the same allocation instead of cloning a fresh
    /// `Vec<Bytes>` per peer.
    SendBody(Arc<Vec<Bytes>>),
    Close,
}

/// Events sent from Peer -> Hub (lifecycle)
#[derive(Debug)]
pub enum HubEvent {
    PeerUp {
        routing_id: Bytes, // Owned + stable
        tx: Sender<PeerCmd>,
    },
    PeerDown {
        routing_id: Bytes,
    },
}

/// Router behavior modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterBehavior {
    /// Standard ROUTER: expects user outbound as [ID, (Empty), Body...]
    /// If ID is unknown, drop silently (libzmq behavior).
    Standard,

    /// Load balancer: expects user outbound as [Body...]
    /// Hub picks a peer using strict-ish RR.
    LoadBalancer,
}

/// The Router Supervisor.
///
/// This runs once per ROUTER socket (listener), and coordinates N peers.
pub struct RouterHub {
    // routing table (keyed by attacker-controlled identity; seeded hasher)
    peers: PeerMap<Sender<PeerCmd>>,

    // LB rotation list (routing IDs)
    lb_list: Vec<Bytes>,
    lb_cursor: usize,
    behavior: RouterBehavior,

    // channels
    hub_rx: Receiver<HubEvent>,
    user_tx_rx: Receiver<RouterCmd>,
}

impl RouterHub {
    #[must_use]
    pub fn new(
        hub_rx: Receiver<HubEvent>,
        user_tx_rx: Receiver<RouterCmd>,
        behavior: RouterBehavior,
    ) -> Self {
        Self {
            peers: PeerMap::default(),
            lb_list: Vec::new(),
            lb_cursor: 0,
            behavior,
            hub_rx,
            user_tx_rx,
        }
    }

    pub async fn run(mut self) {
        use futures::FutureExt;
        use futures::select;

        loop {
            // Use futures::select! for runtime-agnostic multiplexing
            select! {
                msg = self.hub_rx.recv_async().fuse() => {
                    match msg {
                        Ok(ev) => self.handle_peer_event(ev),
                        Err(_) => break, // channel closed
                    }
                }
                msg = self.user_tx_rx.recv_async().fuse() => {
                    match msg {
                        Ok(cmd) => self.handle_user_cmd(cmd),
                        Err(_) => break, // channel closed
                    }
                }
            }
        }

        // Best-effort: close all peers on hub shutdown.
        for tx in self.peers.values() {
            let _ = tx.send(PeerCmd::Close);
        }
    }

    fn handle_peer_event(&mut self, event: HubEvent) {
        match event {
            HubEvent::PeerUp { routing_id, tx } => {
                // Strict dedup: if ID exists, remove it from lb_list first to prevent drift.
                if self.peers.contains_key(&routing_id) {
                    if let Some(pos) = self.lb_list.iter().position(|x| x == &routing_id) {
                        self.lb_list.remove(pos);
                        if self.lb_cursor >= self.lb_list.len() {
                            self.lb_cursor = 0;
                        }
                    }
                }

                // Move routing_id into lb_list, clone for peers map
                self.lb_list.push(routing_id.clone());
                self.peers.insert(routing_id, tx);
            }

            HubEvent::PeerDown { routing_id } => {
                self.peers.remove(&routing_id);

                // Remove from LB list (O(N) but churn is not hot-path).
                if let Some(pos) = self.lb_list.iter().position(|x| x == &routing_id) {
                    self.lb_list.remove(pos);
                    if self.lb_cursor >= self.lb_list.len() {
                        self.lb_cursor = 0;
                    }
                }
            }
        }
    }

    fn handle_user_cmd(&mut self, cmd: RouterCmd) {
        match cmd {
            RouterCmd::SendMessage(parts) => self.route_outbound(parts),
            RouterCmd::Close => {
                // broadcast close to peers
                for tx in self.peers.values() {
                    let _ = tx.send(PeerCmd::Close);
                }
            }
        }
    }

    /// Self-healing Round Robin peer selection.
    ///
    /// Returns a routing id that is present in `peers`, while repairing stale entries in `lb_list`.
    fn pick_rr_peer(&mut self) -> Option<Bytes> {
        let mut attempts = 0usize;
        let max_attempts = self.lb_list.len();

        while !self.lb_list.is_empty() && attempts <= max_attempts {
            if self.lb_cursor >= self.lb_list.len() {
                self.lb_cursor = 0;
            }

            let id = self.lb_list[self.lb_cursor].clone();
            // advance cursor for next pick
            self.lb_cursor = (self.lb_cursor + 1) % self.lb_list.len();

            if self.peers.contains_key(&id) {
                return Some(id);
            }

            // stale entry => repair
            if let Some(pos) = self.lb_list.iter().position(|x| x == &id) {
                self.lb_list.remove(pos);
                // cursor might now be out of bounds; loop header fixes it.
            }

            attempts += 1;
        }

        None
    }

    fn route_outbound(&mut self, mut parts: Vec<Bytes>) {
        if parts.is_empty() {
            return;
        }

        match self.behavior {
            RouterBehavior::Standard => {
                // Expect: [ID, (Empty), Body...]
                // NOTE: `remove(0)` is O(n), but this is hub-path, not IO hot loop.
                let target_id = parts.remove(0);

                // Normalize: drop optional empty delimiter frame
                if !parts.is_empty() && parts[0].is_empty() {
                    parts.remove(0);
                }

                if let Some(tx) = self.peers.get(&target_id) {
                    let _ = tx.send(PeerCmd::SendBody(Arc::new(parts)));
                } else {
                    // ZMQ behavior: silently drop if unknown id
                }
            }

            RouterBehavior::LoadBalancer => {
                // Expect: [Body...]
                if let Some(id) = self.pick_rr_peer() {
                    if let Some(tx) = self.peers.get(&id) {
                        let _ = tx.send(PeerCmd::SendBody(Arc::new(parts)));
                    }
                } else {
                    // No peers available: drop for now (backpressure elsewhere)
                }
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

    /// Receive the next PeerCmd body within a timeout; None on timeout/close.
    async fn recv_body(rx: &Receiver<PeerCmd>) -> Option<Vec<Bytes>> {
        match crate::rt::timeout(Duration::from_secs(1), rx.recv_async()).await {
            Ok(Ok(PeerCmd::SendBody(parts))) => Some((*parts).clone()),
            _ => None,
        }
    }

    /// True if no message body arrives within a short window. A timeout or a
    /// closed channel both count as "no body"; only an actual SendBody fails.
    async fn expect_no_body(rx: &Receiver<PeerCmd>) -> bool {
        !matches!(
            crate::rt::timeout(Duration::from_millis(150), rx.recv_async()).await,
            Ok(Ok(PeerCmd::SendBody(_)))
        )
    }

    #[test]
    fn standard_routes_by_id_strips_envelope_and_drops_unknown() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (hub_tx, hub_rx) = flume::unbounded::<HubEvent>();
            let (user_tx, user_rx) = flume::unbounded::<RouterCmd>();
            let hub = RouterHub::new(hub_rx, user_rx, RouterBehavior::Standard);
            let handle = crate::rt::spawn(hub.run());

            let (peer_a_tx, peer_a_rx) = flume::unbounded::<PeerCmd>();
            hub_tx
                .send(HubEvent::PeerUp {
                    routing_id: b("A"),
                    tx: peer_a_tx,
                })
                .unwrap();
            // Let the hub register the peer before routing to it.
            crate::rt::sleep(Duration::from_millis(30)).await;

            // [ID, empty, body] -> peer receives [body] (id + delimiter stripped).
            user_tx
                .send(RouterCmd::SendMessage(vec![
                    b("A"),
                    Bytes::new(),
                    b("hello"),
                ]))
                .unwrap();
            assert_eq!(recv_body(&peer_a_rx).await, Some(vec![b("hello")]));

            // Unknown id is silently dropped.
            user_tx
                .send(RouterCmd::SendMessage(vec![b("Z"), b("payload")]))
                .unwrap();
            assert!(expect_no_body(&peer_a_rx).await, "unknown id must be dropped");

            drop(hub_tx);
            drop(user_tx);
            crate::rt::join(handle).await;
        });
    }

    #[test]
    fn load_balancer_round_robins_across_peers() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (hub_tx, hub_rx) = flume::unbounded::<HubEvent>();
            let (user_tx, user_rx) = flume::unbounded::<RouterCmd>();
            let hub = RouterHub::new(hub_rx, user_rx, RouterBehavior::LoadBalancer);
            let handle = crate::rt::spawn(hub.run());

            let (peer_a_tx, peer_a_rx) = flume::unbounded::<PeerCmd>();
            let (peer_b_tx, peer_b_rx) = flume::unbounded::<PeerCmd>();
            hub_tx
                .send(HubEvent::PeerUp {
                    routing_id: b("A"),
                    tx: peer_a_tx,
                })
                .unwrap();
            hub_tx
                .send(HubEvent::PeerUp {
                    routing_id: b("B"),
                    tx: peer_b_tx,
                })
                .unwrap();
            crate::rt::sleep(Duration::from_millis(30)).await;

            // Two sends should hit the two peers, one each (round robin).
            user_tx
                .send(RouterCmd::SendMessage(vec![b("m1")]))
                .unwrap();
            user_tx
                .send(RouterCmd::SendMessage(vec![b("m2")]))
                .unwrap();

            let got_a = recv_body(&peer_a_rx).await;
            let got_b = recv_body(&peer_b_rx).await;
            assert!(
                got_a.is_some() && got_b.is_some(),
                "each peer should receive exactly one message under round robin"
            );
            let mut bodies = vec![got_a.unwrap(), got_b.unwrap()];
            bodies.sort();
            assert_eq!(bodies, vec![vec![b("m1")], vec![b("m2")]]);

            drop(hub_tx);
            drop(user_tx);
            crate::rt::join(handle).await;
        });
    }

    #[test]
    fn peer_down_stops_delivery() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (hub_tx, hub_rx) = flume::unbounded::<HubEvent>();
            let (user_tx, user_rx) = flume::unbounded::<RouterCmd>();
            let hub = RouterHub::new(hub_rx, user_rx, RouterBehavior::Standard);
            let handle = crate::rt::spawn(hub.run());

            let (peer_a_tx, peer_a_rx) = flume::unbounded::<PeerCmd>();
            hub_tx
                .send(HubEvent::PeerUp {
                    routing_id: b("A"),
                    tx: peer_a_tx,
                })
                .unwrap();
            crate::rt::sleep(Duration::from_millis(30)).await;
            hub_tx
                .send(HubEvent::PeerDown { routing_id: b("A") })
                .unwrap();
            crate::rt::sleep(Duration::from_millis(30)).await;

            user_tx
                .send(RouterCmd::SendMessage(vec![b("A"), b("late")]))
                .unwrap();
            assert!(
                expect_no_body(&peer_a_rx).await,
                "a downed peer must not receive messages"
            );

            drop(hub_tx);
            drop(user_tx);
            crate::rt::join(handle).await;
        });
    }
}
