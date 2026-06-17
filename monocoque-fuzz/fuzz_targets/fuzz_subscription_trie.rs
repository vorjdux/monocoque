#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use monocoque_core::subscription::{SubscriptionEvent, SubscriptionTrie};

fuzz_target!(|data: &[u8]| {
    let mut trie = SubscriptionTrie::new();

    // Interpret the fuzz input as a sequence of operations.
    // Each operation is encoded as:
    //   byte[0] : operation tag  (0x00 = unsubscribe, 0x01 = subscribe, 0x02 = match,
    //                             0x03 = clear, any other = len-bounded match)
    //   byte[1] : topic length   (0..=63 clamped, to avoid large allocations)
    //   byte[2..2+len] : topic bytes
    //
    // This ensures the fuzzer exercises all public methods of SubscriptionTrie
    // and SubscriptionEvent without hitting OOM from unbounded allocation.
    let mut cursor = data;

    while cursor.len() >= 2 {
        let op = cursor[0];
        let raw_len = cursor[1] as usize;
        // Clamp to 63 bytes per topic so the fuzzer doesn't balloon memory.
        let topic_len = raw_len.min(63).min(cursor.len().saturating_sub(2));
        let topic = &cursor[2..2 + topic_len];
        cursor = &cursor[2 + topic_len..];

        match op {
            0x01 => {
                // subscribe
                let prefix = Bytes::copy_from_slice(topic);
                trie.subscribe(prefix);
            }
            0x00 => {
                // unsubscribe
                let prefix = Bytes::copy_from_slice(topic);
                trie.unsubscribe(&prefix);
            }
            0x02 => {
                // match  -  assert no panic, discard result
                let _ = trie.matches(topic);
            }
            0x03 => {
                // clear
                trie.clear();
            }
            _ => {
                // len / is_empty introspection  -  assert no panic
                let _ = trie.len();
                let _ = trie.is_empty();
                let _ = trie.subscriptions();
            }
        }
    }

    // Always exercise matches on whatever final state the trie is in.
    let _ = trie.matches(data);
    let _ = trie.len();
    let _ = trie.is_empty();

    // Fuzz SubscriptionEvent parsing (from_message / to_message round-trip).
    if !data.is_empty() {
        if let Some(event) = SubscriptionEvent::from_message(data) {
            let encoded = event.to_message();
            // The first byte of the encoded message must be 0x01 or 0x00.
            assert!(encoded[0] == 0x01 || encoded[0] == 0x00);

            // Round-trip: decode again and assert consistency.
            let reparsed = SubscriptionEvent::from_message(&encoded);
            assert!(reparsed.is_some());
        }
    }
});
