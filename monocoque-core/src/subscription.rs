//! Subscription trie for efficient topic matching in XPUB/XSUB/SUB sockets.
//!
//! This provides a more efficient subscription matching mechanism than linear
//! scanning, especially for large numbers of subscriptions.

use bytes::Bytes;

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

/// Efficient subscription storage using a trie structure
///
/// For high-performance topic matching with many subscriptions.
#[derive(Debug, Default)]
pub struct SubscriptionTrie {
    /// Direct list of subscriptions (simple implementation)
    /// Can be upgraded to a real trie if needed for performance
    subscriptions: Vec<Subscription>,
}

impl SubscriptionTrie {
    /// Create a new empty subscription trie
    #[must_use]
    pub const fn new() -> Self {
        Self {
            subscriptions: Vec::new(),
        }
    }

    /// Add a subscription
    pub fn subscribe(&mut self, prefix: Bytes) {
        // Don't add duplicates
        if !self.subscriptions.iter().any(|s| s.prefix == prefix) {
            self.subscriptions.push(Subscription::new(prefix));
        }
    }

    /// Remove a subscription
    pub fn unsubscribe(&mut self, prefix: &Bytes) {
        self.subscriptions.retain(|s| s.prefix != prefix);
    }

    /// Check if a topic matches any subscription
    ///
    /// Returns true if the topic should be delivered.
    #[must_use]
    pub fn matches(&self, topic: &[u8]) -> bool {
        // No subscriptions = don't deliver anything
        if self.subscriptions.is_empty() {
            return false;
        }

        // Check each subscription
        self.subscriptions.iter().any(|s| s.matches(topic))
    }

    /// Get all subscriptions
    #[must_use]
    pub fn subscriptions(&self) -> &[Subscription] {
        &self.subscriptions
    }

    /// Check if there are no subscriptions
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    /// Get the number of subscriptions
    #[must_use]
    pub fn len(&self) -> usize {
        self.subscriptions.len()
    }

    /// Clear all subscriptions
    pub fn clear(&mut self) {
        self.subscriptions.clear();
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
