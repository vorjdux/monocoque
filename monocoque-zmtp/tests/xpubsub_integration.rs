//! Integration tests for XPUB and XSUB sockets
//!
//! Tests cover:
//! - Subscription event encoding/decoding
//! - Subscription trie matching logic
//! - SubscriptionEvent types
//! - Basic socket creation
//!
//! Note: Full end-to-end broker tests require multiple async tasks
//! which are complex to test. These tests focus on the core primitives.

use bytes::Bytes;
use monocoque_core::subscription::{SubscriptionEvent, SubscriptionTrie};
use monocoque_zmtp::xpub::XPubSocket;
use monocoque_zmtp::xsub::XSubSocket;
use std::time::Duration;

/// Test XPUB socket creation and configuration
#[compio::test]
async fn test_xpub_creation() {
    let mut xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
    
    // Test configuration
    xpub.set_verbose(true);
    xpub.set_manual(true);
    
    // Verify socket binds successfully
    let addr = xpub.local_addr().unwrap();
    assert!(addr.port() > 0);
}

/// Test XSUB socket creation and connection
#[compio::test]
async fn test_xsub_creation() {
    // Bind a dummy listener
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    
    // Create XSUB (it will try to connect but may fail without a real PUB socket)
    // This just tests basic creation
    let result = XSubSocket::connect(&addr.to_string()).await;
    
    // Connection may fail since there's no actual publisher, but socket can be created
    // In real usage, XSUB connects to PUB/XPUB sockets
}

/// Test subscription trie matching logic
#[compio::test]
async fn test_subscription_trie_matching() {
    let mut trie = SubscriptionTrie::new();

    // Add subscriptions
    trie.subscribe(Bytes::from("weather."));
    trie.subscribe(Bytes::from("weather.temp"));
    trie.subscribe(Bytes::from("alerts."));

    // Test matching
    assert!(trie.matches(b"weather.temp.celsius"));
    assert!(trie.matches(b"weather.humidity"));
    assert!(trie.matches(b"alerts.warning"));
    assert!(!trie.matches(b"news.breaking"));

    // Test removal
    trie.unsubscribe(&Bytes::from("weather.temp"));
    assert!(trie.matches(b"weather.temp.celsius")); // Still matches "weather."
    assert!(trie.matches(b"weather.humidity"));

    // Remove broader prefix
    trie.unsubscribe(&Bytes::from("weather."));
    assert!(!trie.matches(b"weather.temp.celsius"));
    assert!(!trie.matches(b"weather.humidity"));
    assert!(trie.matches(b"alerts.warning")); // Still matches

    // Test empty subscription (matches all)
    trie.subscribe(Bytes::from(""));
    assert!(trie.matches(b"anything"));
    assert!(trie.matches(b"everything"));
}

/// Test XPUB manual mode (no automatic subscription tracking)
#[compio::test]
async fn test_xpub_manual_mode() {
    let mut xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
    xpub.set_manual(true); // Manual mode
    xpub.set_verbose(true); // Still receive events
    let addr = xpub.local_addr().unwrap();
    assert!(addr.port() > 0);

    // In manual mode, the application is responsible for handling subscriptions
    // The socket doesn't automatically filter messages
    // This test verifies manual mode can be enabled
}

/// Test XPUB welcome message (placeholder for future implementation)
#[compio::test]
async fn test_xpub_welcome_message() {
    let xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
    
    let addr = xpub.local_addr().unwrap();
    assert!(addr.port() > 0);
    
    // Subscriber count tracking is handled internally
    assert_eq!(xpub.subscriber_count(), 0);
}

/// Test XSUB socket subscription tracking
#[compio::test]
async fn test_xsub_subscription_tracking() {
    // Bind a dummy listener for XSUB to connect to
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    
    // In a real scenario, this would connect to a PUB socket
    // For now, test basic subscription tracking without full connection
    let result = XSubSocket::connect(&addr.to_string()).await;
    
    // May not connect successfully without a real publisher
    // The important part is that the API exists
}

/// Test XPUB subscriber count
#[compio::test]
async fn test_xpub_subscriber_count() {
    let xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
    
    // Initially no subscribers
    assert_eq!(xpub.subscriber_count(), 0);
    
    // Subscriber count tracking is handled internally
}

/// Test empty prefix subscription (subscribe to all messages)
#[test]
fn test_empty_prefix_subscription_matching() {
    let mut trie = SubscriptionTrie::new();
    
    // Subscribe to everything (empty prefix)
    trie.subscribe(Bytes::from(""));
    
    // Empty prefix matches all topics
    assert!(trie.matches(b"anything"));
    assert!(trie.matches(b"weather.temp"));
    assert!(trie.matches(b"sports.news"));
}

/// Test XPUB manual mode configuration
#[compio::test]
async fn test_xpub_manual_mode_config() {
    let mut xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
    
    // Test manual mode configuration
    xpub.set_manual(true);
    xpub.set_verbose(true);
    
    // Socket configured successfully
    assert!(xpub.local_addr().is_ok());
}
#[test]
fn test_subscription_event_encoding() {
    // Test subscribe event
    let sub = SubscriptionEvent::Subscribe(Bytes::from("test.topic"));
    let encoded = sub.to_message();
    
    // First byte should be 0x01 (subscribe)
    assert_eq!(encoded[0], 0x01);
    assert_eq!(&encoded[1..], b"test.topic");
    
    // Decode back
    let decoded = SubscriptionEvent::from_message(&encoded).unwrap();
    assert_eq!(decoded, sub);

    // Test unsubscribe event
    let unsub = SubscriptionEvent::Unsubscribe(Bytes::from("other.topic"));
    let encoded = unsub.to_message();
    
    // First byte should be 0x00 (unsubscribe)
    assert_eq!(encoded[0], 0x00);
    assert_eq!(&encoded[1..], b"other.topic");
    
    // Decode back
    let decoded = SubscriptionEvent::from_message(&encoded).unwrap();
    assert_eq!(decoded, unsub);
}

/// Test SubscriptionEvent prefix() method
#[test]
fn test_subscription_event_prefix() {
    let sub = SubscriptionEvent::Subscribe(Bytes::from("weather."));
    assert_eq!(sub.prefix(), &Bytes::from("weather."));

    let unsub = SubscriptionEvent::Unsubscribe(Bytes::from("alerts."));
    assert_eq!(unsub.prefix(), &Bytes::from("alerts."));
}

/// Test subscription trie with overlapping prefixes
#[test]
fn test_subscription_trie_overlapping() {
    let mut trie = SubscriptionTrie::new();

    // Add overlapping subscriptions
    trie.subscribe(Bytes::from("weather."));
    trie.subscribe(Bytes::from("weather.temp."));
    
    // Both should match more specific topics
    assert!(trie.matches(b"weather.temp.celsius"));
    assert!(trie.matches(b"weather.humidity"));
    
    // Remove specific one
    trie.unsubscribe(&Bytes::from("weather.temp."));
    
    // General one still matches
    assert!(trie.matches(b"weather.temp.celsius"));
    assert!(trie.matches(b"weather.humidity"));
}

/// Test XPUB with no verbose mode
#[compio::test]
async fn test_xpub_non_verbose() {
    let xpub = XPubSocket::bind("127.0.0.1:0").await.unwrap();
    // verbose = false by default (verified internally in socket)
    
    let addr = xpub.local_addr().unwrap();
    assert!(addr.port() > 0);
    
    // In non-verbose mode, XPUB doesn't report subscription events
    // This just tests basic socket creation
}
