//! PUB/SUB Subscription Index (Phase 3: Sorted Prefix Table)
//!
//! Design:
//! - Keep subscriptions in a Vec sorted lexicographically by `prefix`.
//! - subscribe/unsubscribe: O(log N) search + O(N) insert/remove shift (N ~ < 10k typical).
//! - `match_topic` hot-path: cache-friendly forward scan with early-exit when prefix > topic.
//! - Returns `SmallVec` of `PeerKeys` to avoid heap alloc in common cases.
//! - Dedups results because peers may subscribe to overlapping prefixes.

use bytes::Bytes;
use smallvec::SmallVec;

/// Compact integer ID for peers to keep the index cache-dense.
/// (Avoids storing Bytes/Senders directly in the hot structure.)
pub type PeerKey = u64;

#[derive(Debug, Clone)]
struct Subscription {
    prefix: Bytes,
    /// Inline up to 4 peers without heap allocation (common low fanout).
    peers: SmallVec<[PeerKey; 4]>,
}

#[derive(Debug, Default)]
pub struct SubscriptionIndex {
    subs: Vec<Subscription>,
}

impl SubscriptionIndex {
    #[must_use] 
    pub const fn new() -> Self {
        Self { subs: Vec::new() }
    }

    #[inline]
    #[must_use]
    #[allow(clippy::incompatible_msrv)] // is_empty in const context requires 1.87
    pub const fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }

    /// Adds a subscription for `peer` to `prefix`.
    ///
    /// Complexity:
    /// - O(log N) to find
    /// - O(N) for insertion shift in the vec (acceptable for typical ZMQ sizes)
    pub fn subscribe(&mut self, peer: PeerKey, prefix: Bytes) {
        match self.subs.binary_search_by(|s| s.prefix.cmp(&prefix)) {
            Ok(idx) => {
                let peers = &mut self.subs[idx].peers;
                if !peers.contains(&peer) {
                    peers.push(peer);
                }
            }
            Err(idx) => {
                let mut peers = SmallVec::<[PeerKey; 4]>::new();
                peers.push(peer);
                self.subs.insert(idx, Subscription { prefix, peers });
            }
        }
    }

    /// Removes a subscription for `peer` from `prefix`.
    pub fn unsubscribe(&mut self, peer: PeerKey, prefix: &Bytes) {
        if let Ok(idx) = self.subs.binary_search_by(|s| s.prefix.cmp(prefix)) {
            let peers = &mut self.subs[idx].peers;
            if let Some(pos) = peers.iter().position(|p| *p == peer) {
                peers.swap_remove(pos);
            }
            if peers.is_empty() {
                self.subs.remove(idx);
            }
        }
    }

    /// Remove `peer` from every prefix (used on disconnect).
    ///
    /// Complexity: O(N) scan, acceptable on churn events.
    pub fn remove_peer_everywhere(&mut self, peer: PeerKey) {
        let mut i = 0usize;
        while i < self.subs.len() {
            let peers = &mut self.subs[i].peers;

            if let Some(pos) = peers.iter().position(|p| *p == peer) {
                peers.swap_remove(pos);
            }

            if peers.is_empty() {
                self.subs.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Match a topic against all subscriptions.
    ///
    /// Returns a deduplicated list of `PeerKeys`.
    ///
    /// Hot path characteristics:
    /// - Forward scan over sorted prefixes.
    /// - Early exit when prefix > topic lexicographically (cannot be a prefix).
    /// - Starts-with check for actual prefix match.
    #[must_use] 
    pub fn match_topic(&self, topic: &[u8]) -> SmallVec<[PeerKey; 16]> {
        let mut out: SmallVec<[PeerKey; 16]> = SmallVec::new();

        for sub in &self.subs {
            let p = sub.prefix.as_ref();

            // If prefix > topic, it cannot be a prefix of topic.
            // e.g. prefix="apply" > topic="apple" => stop.
            if p > topic {
                break;
            }

            if topic.starts_with(p) {
                out.extend_from_slice(&sub.peers);
            }
        }

        // Dedup if needed (peer might have subscribed to nested prefixes).
        if out.len() > 1 {
            out.sort_unstable();
            out.dedup();
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_and_match() {
        let mut idx = SubscriptionIndex::new();

        idx.subscribe(1, Bytes::from_static(b"A"));
        idx.subscribe(2, Bytes::from_static(b"AB"));
        idx.subscribe(3, Bytes::from_static(b"B"));

        let m = idx.match_topic(b"ABC");
        assert_eq!(m.as_slice(), &[1, 2]);

        let m = idx.match_topic(b"BANANA");
        assert_eq!(m.as_slice(), &[3]);
    }

    #[test]
    fn dedup_nested_prefixes() {
        let mut idx = SubscriptionIndex::new();

        idx.subscribe(7, Bytes::from_static(b"A"));
        idx.subscribe(7, Bytes::from_static(b"AB"));

        let m = idx.match_topic(b"ABCD");
        assert_eq!(m.as_slice(), &[7]);
    }

    #[test]
    fn remove_peer_everywhere_cleans_empty_entries() {
        let mut idx = SubscriptionIndex::new();

        idx.subscribe(1, Bytes::from_static(b"A"));
        idx.subscribe(2, Bytes::from_static(b"A"));
        idx.subscribe(1, Bytes::from_static(b"AB"));

        idx.remove_peer_everywhere(1);

        let m = idx.match_topic(b"ABCD");
        assert_eq!(m.as_slice(), &[2]);
    }
}
