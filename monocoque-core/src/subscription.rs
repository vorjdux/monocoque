//! Subscription matching: the one matcher behind PUB, XPUB, SUB and XSUB.
//!
//! [`SubscriptionIndex`] keeps every subscribed prefix in a vector sorted
//! lexicographically, each with the peers holding it and a per-peer count (a
//! peer that subscribes twice must cancel twice, as in libzmq). Matching a topic
//! scans forward and stops at the first prefix that sorts after the topic,
//! since nothing past it can be a prefix of it. [`SubscriptionTrie`] is the
//! same index with a single subscriber, used for local filtering.

use bytes::{Bytes, BytesMut};
use smallvec::SmallVec;
use std::collections::HashMap;

/// Compact peer key stored in the index.
pub type PeerKey = u64;

/// Distinct prefixes one peer may hold before further subscriptions are
/// ignored: a memory-amplification bound on an untrusted subscriber.
pub const MAX_PREFIXES_PER_PEER: usize = 10_000;

#[derive(Debug, Clone)]
struct Entry {
    prefix: Bytes,
    peers: SmallVec<[(PeerKey, u32); 2]>,
}

/// What [`SubscriptionIndex::subscribe`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscribed {
    /// No peer held this prefix before: upstream should hear about it.
    FirstForPrefix,
    /// The prefix existed; this peer took its first reference to it.
    FirstForPeer,
    /// This peer already held the prefix; its count went up.
    Repeated,
    /// The peer is at [`MAX_PREFIXES_PER_PEER`]; nothing changed.
    Rejected,
}

/// What [`SubscriptionIndex::unsubscribe`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsubscribed {
    /// The last reference anywhere went: upstream should hear about it.
    LastForPrefix,
    /// This peer dropped its last reference; others still hold the prefix.
    LastForPeer,
    /// The peer still holds other references.
    Decremented,
    /// The peer did not hold the prefix.
    NotSubscribed,
}

/// Prefix subscriptions of many peers. See the [module docs](self).
#[derive(Debug, Default)]
pub struct SubscriptionIndex {
    entries: Vec<Entry>,
    per_peer: HashMap<PeerKey, usize>,
}

impl SubscriptionIndex {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when no prefix is subscribed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of distinct prefixes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Distinct prefixes `peer` holds.
    #[must_use]
    pub fn prefixes_of(&self, peer: PeerKey) -> usize {
        self.per_peer.get(&peer).copied().unwrap_or(0)
    }

    /// Add one reference from `peer` to `prefix`.
    pub fn subscribe(&mut self, peer: PeerKey, prefix: Bytes) -> Subscribed {
        match self.entries.binary_search_by(|e| e.prefix.cmp(&prefix)) {
            Ok(i) => {
                if let Some(slot) = self.entries[i].peers.iter_mut().find(|(p, _)| *p == peer) {
                    slot.1 = slot.1.saturating_add(1);
                    return Subscribed::Repeated;
                }
                if self.prefixes_of(peer) >= MAX_PREFIXES_PER_PEER {
                    return Subscribed::Rejected;
                }
                self.entries[i].peers.push((peer, 1));
                *self.per_peer.entry(peer).or_default() += 1;
                Subscribed::FirstForPeer
            }
            Err(i) => {
                if self.prefixes_of(peer) >= MAX_PREFIXES_PER_PEER {
                    return Subscribed::Rejected;
                }
                let mut peers = SmallVec::new();
                peers.push((peer, 1));
                self.entries.insert(i, Entry { prefix, peers });
                *self.per_peer.entry(peer).or_default() += 1;
                Subscribed::FirstForPrefix
            }
        }
    }

    /// Drop one reference from `peer` to `prefix`.
    pub fn unsubscribe(&mut self, peer: PeerKey, prefix: &[u8]) -> Unsubscribed {
        let Ok(i) = self
            .entries
            .binary_search_by(|e| e.prefix.as_ref().cmp(prefix))
        else {
            return Unsubscribed::NotSubscribed;
        };
        let peers = &mut self.entries[i].peers;
        let Some(pos) = peers.iter().position(|(p, _)| *p == peer) else {
            return Unsubscribed::NotSubscribed;
        };
        if peers[pos].1 > 1 {
            peers[pos].1 -= 1;
            return Unsubscribed::Decremented;
        }
        peers.swap_remove(pos);
        let now_empty = peers.is_empty();
        self.release_peer_slot(peer);
        if now_empty {
            self.entries.remove(i);
            Unsubscribed::LastForPrefix
        } else {
            Unsubscribed::LastForPeer
        }
    }

    fn release_peer_slot(&mut self, peer: PeerKey) {
        if let Some(n) = self.per_peer.get_mut(&peer) {
            *n -= 1;
            if *n == 0 {
                self.per_peer.remove(&peer);
            }
        }
    }

    /// Remove every subscription of `peer`. Returns the prefixes no peer holds
    /// any more, which an XPUB forwards upstream as unsubscriptions.
    pub fn remove_peer(&mut self, peer: PeerKey) -> Vec<Bytes> {
        let mut gone = Vec::new();
        if self.per_peer.remove(&peer).is_none() {
            return gone;
        }
        self.entries.retain_mut(|e| {
            if let Some(pos) = e.peers.iter().position(|(p, _)| *p == peer) {
                e.peers.swap_remove(pos);
            }
            if e.peers.is_empty() {
                gone.push(e.prefix.clone());
                false
            } else {
                true
            }
        });
        gone
    }

    /// Append to `out` every peer subscribed to a prefix of `topic`, each once.
    pub fn match_peers(&self, topic: &[u8], out: &mut SmallVec<[PeerKey; 16]>) {
        out.clear();
        for e in &self.entries {
            let p = e.prefix.as_ref();
            if p > topic {
                break;
            }
            if topic.starts_with(p) {
                out.extend(e.peers.iter().map(|(peer, _)| *peer));
            }
        }
        if out.len() > 1 {
            out.sort_unstable();
            out.dedup();
        }
    }

    /// `true` if `peer` holds a prefix of `topic`.
    #[must_use]
    pub fn peer_matches(&self, peer: PeerKey, topic: &[u8]) -> bool {
        for e in &self.entries {
            let p = e.prefix.as_ref();
            if p > topic {
                return false;
            }
            if topic.starts_with(p) && e.peers.iter().any(|(q, _)| *q == peer) {
                return true;
            }
        }
        false
    }

    /// `true` if any peer holds a prefix of `topic`.
    #[must_use]
    pub fn matches_any(&self, topic: &[u8]) -> bool {
        for e in &self.entries {
            let p = e.prefix.as_ref();
            if p > topic {
                return false;
            }
            if topic.starts_with(p) {
                return true;
            }
        }
        false
    }

    /// Every distinct prefix, sorted.
    pub fn prefixes(&self) -> impl Iterator<Item = &Bytes> {
        self.entries.iter().map(|e| &e.prefix)
    }

    /// Remove everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.per_peer.clear();
    }
}

/// The subscriptions of one local socket (SUB, XSUB): a
/// [`SubscriptionIndex`] with a single subscriber.
#[derive(Debug, Default)]
pub struct SubscriptionTrie {
    index: SubscriptionIndex,
}

const LOCAL: PeerKey = 0;

/// A subscription entry with topic prefix
#[derive(Debug, Clone)]
pub struct Subscription {
    /// Topic prefix (empty = subscribe to all)
    pub prefix: Bytes,
}

impl Subscription {
    /// Create a new subscription for a topic prefix
    #[must_use]
    pub const fn new(prefix: Bytes) -> Self {
        Self { prefix }
    }

    /// Check if this subscription matches a given topic
    #[must_use]
    pub fn matches(&self, topic: &[u8]) -> bool {
        // Empty prefix matches everything
        if self.prefix.is_empty() {
            return true;
        }

        // Check if topic starts with prefix
        topic.len() >= self.prefix.len() && topic[..self.prefix.len()] == self.prefix[..]
    }
}

impl SubscriptionTrie {
    /// An empty subscription set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a reference to `prefix`. Returns `true` if it was not held before.
    pub fn subscribe(&mut self, prefix: Bytes) -> bool {
        matches!(
            self.index.subscribe(LOCAL, prefix),
            Subscribed::FirstForPrefix
        )
    }

    /// Drop a reference to `prefix`. Returns `true` if that was the last one.
    pub fn unsubscribe(&mut self, prefix: &[u8]) -> bool {
        matches!(
            self.index.unsubscribe(LOCAL, prefix),
            Unsubscribed::LastForPrefix
        )
    }

    /// `true` if some subscribed prefix is a prefix of `topic`.
    #[must_use]
    pub fn matches(&self, topic: &[u8]) -> bool {
        self.index.matches_any(topic)
    }

    /// Every subscribed prefix.
    #[must_use]
    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.index
            .prefixes()
            .map(|p| Subscription::new(p.clone()))
            .collect()
    }

    /// `true` when nothing is subscribed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Number of distinct subscribed prefixes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Remove every subscription.
    pub fn clear(&mut self) {
        self.index.clear();
    }
}

/// Check whether a topic matches any subscription prefix.
///
/// An empty prefix matches all topics. An empty prefix list matches none.
#[must_use]
pub fn topic_matches_prefixes(topic: &[u8], prefixes: &[Bytes]) -> bool {
    prefixes
        .iter()
        .any(|prefix| prefix.is_empty() || topic.starts_with(prefix))
}

/// Subscription event for XPUB socket
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionEvent {
    /// A peer subscribed to a topic
    Subscribe(Bytes),
    /// A peer unsubscribed from a topic
    Unsubscribe(Bytes),
}

impl SubscriptionEvent {
    /// Create a subscription event from a ZMTP subscription message
    ///
    /// Format: [0x01|0x00] [topic prefix...]
    #[must_use]
    pub fn from_message(msg: &[u8]) -> Option<Self> {
        if msg.is_empty() {
            return None;
        }

        let prefix = Bytes::copy_from_slice(&msg[1..]);
        match msg[0] {
            0x01 => Some(Self::Subscribe(prefix)),
            0x00 => Some(Self::Unsubscribe(prefix)),
            _ => None,
        }
    }

    /// Create a subscription event from an owned payload without copying the prefix.
    #[must_use]
    pub fn from_bytes(msg: Bytes) -> Option<Self> {
        if msg.is_empty() {
            return None;
        }

        let prefix = msg.slice(1..);
        match msg[0] {
            0x01 => Some(Self::Subscribe(prefix)),
            0x00 => Some(Self::Unsubscribe(prefix)),
            _ => None,
        }
    }

    /// Encode this event as a ZMTP subscription message
    #[must_use]
    pub fn to_message(&self) -> Bytes {
        let (cmd, prefix) = match self {
            Self::Subscribe(p) => (0x01u8, p),
            Self::Unsubscribe(p) => (0x00u8, p),
        };

        let mut msg = BytesMut::with_capacity(1 + prefix.len());
        msg.extend_from_slice(&[cmd]);
        msg.extend_from_slice(prefix);
        msg.freeze()
    }

    /// Get the topic prefix
    #[must_use]
    pub const fn prefix(&self) -> &Bytes {
        match self {
            Self::Subscribe(p) | Self::Unsubscribe(p) => p,
        }
    }

    /// Check if this is a subscribe event
    #[must_use]
    pub const fn is_subscribe(&self) -> bool {
        matches!(self, Self::Subscribe(_))
    }

    /// Check if this is an unsubscribe event
    #[must_use]
    pub const fn is_unsubscribe(&self) -> bool {
        matches!(self, Self::Unsubscribe(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_matches() {
        let sub = Subscription::new(Bytes::from_static(b"topic."));

        assert!(sub.matches(b"topic.foo"));
        assert!(sub.matches(b"topic.bar"));
        assert!(!sub.matches(b"other.foo"));
        assert!(!sub.matches(b"topi"));
    }

    #[test]
    fn test_empty_subscription_matches_all() {
        let sub = Subscription::new(Bytes::new());

        assert!(sub.matches(b"anything"));
        assert!(sub.matches(b""));
    }

    #[test]
    fn test_trie_basic() {
        let mut trie = SubscriptionTrie::new();

        assert!(!trie.matches(b"topic.foo"));

        trie.subscribe(Bytes::from_static(b"topic."));
        assert!(trie.matches(b"topic.foo"));
        assert!(!trie.matches(b"other.foo"));

        trie.unsubscribe(&Bytes::from_static(b"topic."));
        assert!(!trie.matches(b"topic.foo"));
    }

    #[test]
    fn test_trie_multiple_subscriptions() {
        let mut trie = SubscriptionTrie::new();

        trie.subscribe(Bytes::from_static(b"topic."));
        trie.subscribe(Bytes::from_static(b"events."));

        assert!(trie.matches(b"topic.foo"));
        assert!(trie.matches(b"events.bar"));
        assert!(!trie.matches(b"other.baz"));
    }

    #[test]
    fn test_trie_empty_prefix_matches_all() {
        let mut trie = SubscriptionTrie::new();
        trie.subscribe(Bytes::new());

        assert!(trie.matches(b"anything"));
        assert!(trie.matches(b""));
    }

    #[test]
    fn test_trie_no_false_prefix_match() {
        let mut trie = SubscriptionTrie::new();
        trie.subscribe(Bytes::from_static(b"topic."));

        // "topic" is a prefix of "topic." but "topic." is NOT a prefix of "topic"
        assert!(!trie.matches(b"topic"));
        assert!(trie.matches(b"topic."));
        assert!(trie.matches(b"topic.sub"));
    }

    #[test]
    fn test_trie_checks_broader_prefix_after_non_matching_candidate() {
        let mut trie = SubscriptionTrie::new();
        trie.subscribe(Bytes::from_static(b"a"));
        trie.subscribe(Bytes::from_static(b"aa~"));

        assert!(trie.matches(b"ab"));
    }

    #[test]
    fn index_matches_each_peer_once() {
        let mut idx = SubscriptionIndex::new();
        idx.subscribe(1, Bytes::from_static(b"A"));
        idx.subscribe(2, Bytes::from_static(b"AB"));
        idx.subscribe(3, Bytes::from_static(b"B"));
        idx.subscribe(1, Bytes::from_static(b"AB"));
        let mut out = SmallVec::new();
        idx.match_peers(b"ABC", &mut out);
        assert_eq!(out.as_slice(), &[1, 2]);
        idx.match_peers(b"BANANA", &mut out);
        assert_eq!(out.as_slice(), &[3]);
        assert!(idx.peer_matches(2, b"ABX"));
        assert!(!idx.peer_matches(3, b"ABX"));
    }

    #[test]
    fn index_counts_references_per_peer() {
        let mut idx = SubscriptionIndex::new();
        let a = Bytes::from_static(b"a");
        assert_eq!(idx.subscribe(1, a.clone()), Subscribed::FirstForPrefix);
        assert_eq!(idx.subscribe(1, a.clone()), Subscribed::Repeated);
        assert_eq!(idx.subscribe(2, a), Subscribed::FirstForPeer);
        assert_eq!(idx.unsubscribe(1, b"a"), Unsubscribed::Decremented);
        assert_eq!(idx.unsubscribe(1, b"a"), Unsubscribed::LastForPeer);
        assert_eq!(idx.unsubscribe(1, b"a"), Unsubscribed::NotSubscribed);
        assert_eq!(idx.unsubscribe(2, b"a"), Unsubscribed::LastForPrefix);
        assert!(idx.is_empty());
    }

    #[test]
    fn removing_a_peer_reports_prefixes_nobody_holds() {
        let mut idx = SubscriptionIndex::new();
        idx.subscribe(1, Bytes::from_static(b"A"));
        idx.subscribe(2, Bytes::from_static(b"A"));
        idx.subscribe(1, Bytes::from_static(b"AB"));
        let gone = idx.remove_peer(1);
        assert_eq!(gone, vec![Bytes::from_static(b"AB")]);
        let mut out = SmallVec::new();
        idx.match_peers(b"ABCD", &mut out);
        assert_eq!(out.as_slice(), &[2]);
        assert_eq!(idx.prefixes_of(1), 0);
    }

    #[test]
    fn per_peer_prefix_count_is_capped() {
        let mut idx = SubscriptionIndex::new();
        for i in 0..MAX_PREFIXES_PER_PEER {
            let p = Bytes::from(format!("t{i}"));
            assert_ne!(idx.subscribe(9, p), Subscribed::Rejected);
        }
        assert_eq!(
            idx.subscribe(9, Bytes::from_static(b"one-too-many")),
            Subscribed::Rejected
        );
        assert_eq!(idx.prefixes_of(9), MAX_PREFIXES_PER_PEER);
        // Another peer is unaffected.
        assert_eq!(
            idx.subscribe(10, Bytes::from_static(b"one-too-many")),
            Subscribed::FirstForPrefix
        );
    }

    #[test]
    fn test_subscription_event() {
        let sub = SubscriptionEvent::Subscribe(Bytes::from_static(b"topic"));
        let msg = sub.to_message();

        assert_eq!(msg[0], 0x01);
        assert_eq!(&msg[1..], b"topic");

        let parsed = SubscriptionEvent::from_message(&msg).unwrap();
        assert_eq!(parsed, sub);
    }

    #[test]
    fn test_unsubscription_event() {
        let unsub = SubscriptionEvent::Unsubscribe(Bytes::from_static(b"topic"));
        let msg = unsub.to_message();

        assert_eq!(msg[0], 0x00);
        assert_eq!(&msg[1..], b"topic");

        let parsed = SubscriptionEvent::from_message(&msg).unwrap();
        assert_eq!(parsed, unsub);
    }
}
