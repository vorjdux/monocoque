//! Subscription trie for efficient topic matching in XPUB/XSUB/SUB sockets.
//!
//! This provides a more efficient subscription matching mechanism than linear
//! scanning, especially for large numbers of subscriptions.

use bytes::Bytes;
use std::collections::BTreeSet;

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

/// Efficient subscription storage using a sorted set for O(log N) operations.
///
/// Backed by a `BTreeSet<Vec<u8>>` which allows O(log N) prefix searching:
/// find the largest stored prefix ≤ the topic, then check if it is a prefix
/// of the topic.  Subscribe/unsubscribe are also O(log N).
#[derive(Debug, Default)]
pub struct SubscriptionTrie {
    prefixes: BTreeSet<Vec<u8>>,
}

impl SubscriptionTrie {
    /// Create a new empty subscription trie
    #[must_use]
    pub fn new() -> Self {
        Self {
            prefixes: BTreeSet::new(),
        }
    }

    /// Add a subscription
    pub fn subscribe(&mut self, prefix: Bytes) {
        self.prefixes.insert(prefix.to_vec());
    }

    /// Remove a subscription
    pub fn unsubscribe(&mut self, prefix: &Bytes) {
        self.prefixes.remove(prefix.as_ref());
    }

    /// Check if a topic matches any subscription
    ///
    /// Returns true if the topic should be delivered.
    /// O(log N) using `BTreeSet` range lookup.
    #[must_use]
    pub fn matches(&self, topic: &[u8]) -> bool {
        if self.prefixes.is_empty() {
            return false;
        }

        // Check empty prefix first (matches everything)
        if self.prefixes.contains(&[][..]) {
            return true;
        }

        // Find the largest stored prefix <= topic.
        // Any stored prefix that is a true prefix of `topic` must be <= topic
        // in lexicographic order, so the best candidate is the largest such key.
        use std::ops::Bound;
        if let Some(candidate) = self
            .prefixes
            .range::<Vec<u8>, _>((Bound::Unbounded, Bound::Included(&topic.to_vec())))
            .next_back()
        {
            if topic.starts_with(candidate.as_slice()) {
                return true;
            }
        }

        false
    }

    /// Get all subscriptions as a `Vec<Subscription>`.
    #[must_use]
    pub fn subscriptions(&self) -> Vec<Subscription> {
        self.prefixes
            .iter()
            .map(|p| Subscription::new(Bytes::copy_from_slice(p)))
            .collect()
    }

    /// Check if there are no subscriptions
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty()
    }

    /// Get the number of subscriptions
    #[must_use]
    pub fn len(&self) -> usize {
        self.prefixes.len()
    }

    /// Clear all subscriptions
    pub fn clear(&mut self) {
        self.prefixes.clear();
    }
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

    /// Encode this event as a ZMTP subscription message
    #[must_use]
    pub fn to_message(&self) -> Bytes {
        let (cmd, prefix) = match self {
            Self::Subscribe(p) => (0x01u8, p),
            Self::Unsubscribe(p) => (0x00u8, p),
        };

        let mut msg = Vec::with_capacity(1 + prefix.len());
        msg.push(cmd);
        msg.extend_from_slice(prefix);
        Bytes::from(msg)
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
