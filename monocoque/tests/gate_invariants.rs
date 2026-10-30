//! GATE tests: invariants the multiplexer and its patterns must preserve.
//!
//! Each test asserts the desired invariant through public behavior. All four
//! were placeholders on the single-peer design, where the state they guard was
//! not observable; on the multiplexer every one is enforced and checked.

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{
    MonocoqueError, Msg, PeerId, PubSocket, PullSocket, PushSocket, SocketOptions, SubSocket,
};
use std::time::Duration;

fn quick(ms: u64) -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_millis(ms))
}

async fn wait_for(mut cond: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if cond() {
            return;
        }
        monocoque::rt::sleep(Duration::from_millis(1)).await;
    }
    panic!("condition not reached");
}

/// Invariant 1: a peer that streams an unbounded number of MORE frames in one
/// logical message is disconnected, not accumulated without bound.
#[test]
fn multipart_frame_count_is_capped() {
    const PROBE_FRAMES: usize = 50_000;
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(quick(500));
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        let abusive: Msg = (0..PROBE_FRAMES)
            .map(|_| Bytes::from_static(b"x"))
            .collect();
        push.send(abusive).await.unwrap();
        // The reader rejects the message at the cap and drops the connection,
        // so nothing is delivered.
        assert!(matches!(pull.recv().await, Err(MonocoqueError::TimedOut)));
        // PUSH reconnects (as it should); a well-formed message then arrives,
        // so the connection was reset rather than left wedged.
        push.send("after").await.unwrap();
        assert_eq!(pull.recv().await.unwrap(), [b"after"]);
    });
}

/// Invariant 2: when a subscriber disconnects, the publisher frees all of its
/// subscription state.
#[test]
fn subscription_state_freed_on_disconnect() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::new();
        sub.subscribe(b"topic").await.unwrap();
        sub.subscribe(b"other").await.unwrap();
        sub.connect(&ep).await.unwrap();
        wait_for(|| publisher.peer_count() == 1).await;
        let peer: PeerId = publisher.peer_ids()[0];
        // Publishing drains pending subscription messages first.
        wait_for(|| {
            futures::executor::block_on(publisher.send("topic.x")).unwrap();
            publisher.subscriber_prefixes(peer) == 2
        })
        .await;
        drop(sub);
        wait_for(|| publisher.peer_count() == 0).await;
        assert_eq!(
            publisher.subscriber_prefixes(peer),
            0,
            "the departed subscriber's prefixes leaked"
        );
    });
}

/// Invariant 3: a subscriber cannot register an unbounded number of distinct
/// prefixes.
#[test]
fn per_subscriber_prefix_count_is_capped() {
    use monocoque_core::subscription::MAX_PREFIXES_PER_PEER;
    LocalRuntime::new().unwrap().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::new();
        sub.connect(&ep).await.unwrap();
        wait_for(|| publisher.peer_count() == 1).await;
        let peer = publisher.peer_ids()[0];
        for i in 0..MAX_PREFIXES_PER_PEER + 500 {
            sub.subscribe(format!("p{i}")).await.unwrap();
        }
        wait_for(|| {
            futures::executor::block_on(publisher.send("p0")).unwrap();
            publisher.subscriber_prefixes(peer) >= MAX_PREFIXES_PER_PEER
        })
        .await;
        monocoque::rt::sleep(Duration::from_millis(50)).await;
        futures::executor::block_on(publisher.send("p0")).unwrap();
        assert_eq!(publisher.subscriber_prefixes(peer), MAX_PREFIXES_PER_PEER);
    });
}

/// Invariant 4: a peer slot reused after a disconnect never inherits the old
/// peer's subscriptions. Peer ids carry a generation, so the departed peer's
/// id is distinct from its successor's even when they share a slot.
#[test]
fn reused_peer_slot_starts_with_no_subscriptions() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();

        let mut first = SubSocket::new();
        first.subscribe(b"old").await.unwrap();
        first.connect(&ep).await.unwrap();
        wait_for(|| publisher.peer_count() == 1).await;
        let old_id = publisher.peer_ids()[0];
        wait_for(|| {
            futures::executor::block_on(publisher.send("old")).unwrap();
            publisher.subscriber_prefixes(old_id) == 1
        })
        .await;
        drop(first);
        wait_for(|| publisher.peer_count() == 0).await;

        let mut second = SubSocket::with_options(quick(200));
        second.connect(&ep).await.unwrap();
        wait_for(|| publisher.peer_count() == 1).await;
        let new_id = publisher.peer_ids()[0];
        assert_eq!(new_id.index(), old_id.index(), "the slot was reused");
        assert_ne!(new_id, old_id, "a reused slot must get a new identity");
        assert_eq!(publisher.subscriber_prefixes(new_id), 0);
        // The new subscriber never subscribed, so it gets nothing.
        publisher.send("old").await.unwrap();
        assert!(matches!(second.recv().await, Err(MonocoqueError::TimedOut)));
    });
}
