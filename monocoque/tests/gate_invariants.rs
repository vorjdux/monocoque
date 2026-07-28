//! Phase 4 rework GATE tests: invariants the reworked internals must preserve.
//!
//! HONESTY CONTRACT: each test asserts the DESIRED invariant, never the current
//! (possibly buggy) behavior. Tests that pass on today's code are active. Tests
//! that guard a bug not fixed until a later phase are `#[ignore]`d with a reason
//! string, so the suite is green today and the guard activates when the fix
//! lands. See the module report at the bottom for the enforced-vs-unenforced
//! status of each invariant on the code this file was written against.

use bytes::Bytes;
use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{PubSocket, PullSocket, PushSocket, SocketOptions, SubSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn opts() -> SocketOptions {
    SocketOptions::default().with_buffer_sizes(16384, 16384)
}

// ─────────────────────────────────────────────────────────────────────────────
// Invariant 1: frame-count / aggregate-size cap on multipart recv.
//
// A peer that streams an unbounded number of MORE frames in one logical message
// must be rejected or have its connection closed, NOT accumulated without bound.
//
// STATUS: CURRENTLY UNENFORCED. The DEALER/PULL decode loop
// (monocoque-zmtp/src/dealer.rs, base.rs) pushes every framed payload into an
// unbounded `frames` accumulator and only completes the message when the MORE
// bit clears; there is a per-FRAME size cap (`with_max_frame_size`) but no cap on
// the frame COUNT or the aggregate size of one logical multipart message. So a
// single message with a huge frame count is accumulated in full. This test
// asserts the desired cap and is therefore `#[ignore]`d until the Phase 1 fix.
// ─────────────────────────────────────────────────────────────────────────────

/// Frames in the oversized probe message. Far above any sane per-message cap.
const PROBE_FRAMES: usize = 50_000;
/// A sane ceiling the reworked decoder should enforce on a single message.
const SANE_FRAME_CAP: usize = 1024;

#[test]
#[ignore = "guards multipart frame-count/aggregate-size cap; currently unenforced (unbounded frames accumulator in the ZMTP decode loop), activate when the Phase 1 fix lands"]
fn multipart_frame_count_is_capped() {
    let (port_tx, port_rx) = mpsc::channel::<u16>();

    let sender = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let port = port_rx.recv().unwrap();
            let mut push = PushSocket::connect_with_options(("127.0.0.1", port), opts())
                .await
                .unwrap();
            // One logical message carrying an abusive number of frames.
            let msg: Vec<Bytes> = (0..PROBE_FRAMES).map(|_| Bytes::from_static(b"x")).collect();
            let _ = push.send(msg).await;
            let _ = push.flush().await;
            // Keep the socket alive briefly so the peer can react.
            thread::sleep(Duration::from_millis(200));
        });
    });

    let rt = LocalRuntime::new().unwrap();
    rt.block_on(async move {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        port_tx.send(listener.local_addr().unwrap().port()).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut pull = PullSocket::from_tcp_with_options(stream, opts()).await.unwrap();

        // DESIRED: the abusive message is rejected/closed, or at worst delivered
        // truncated to a sane cap - never accumulated in full.
        match pull.recv().await {
            Ok(Some(msg)) => assert!(
                msg.len() <= SANE_FRAME_CAP,
                "a single logical message accumulated {} frames without bound \
                 (cap {SANE_FRAME_CAP}); the decoder must reject or close instead",
                msg.len()
            ),
            Ok(None) | Err(_) => { /* rejected/closed: the invariant holds */ }
        }
    });

    sender.join().unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// Invariant 2: subscription state fully freed on subscriber disconnect.
//
// After a SUB disconnects, the PUB must drop its subscription/prefix state for
// that peer (no leak).
//
// STATUS: structurally handled inside the worker pool
// (monocoque-zmtp/src/publisher.rs `worker_thread` evicts a dead subscriber with
// `subscribers.remove(&id)` and `sub_count.fetch_sub(..)`, and the subscription
// reader drops the peer's union prefixes on EOF), but NOT observable through the
// public `PubSocket` API: `subscriber_count()` is a plain field that is only
// decremented by `remove_subscriber`, which the public wrapper does not call on
// disconnect. So there is no public signal to assert the freed state against.
// Per the honesty contract this is an `#[ignore]`d documented placeholder rather
// than a fake assertion; it becomes a unit test on the internal worker state in
// the rework. The body sets up the real disconnect scenario so the guard is
// ready to assert once a public accessor (or the unit-level hook) exists.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "guards subscription-state-freed-on-disconnect; eviction is structural in the worker pool but not observable through the public PubSocket API, so it becomes a unit test in the rework"]
fn subscription_state_freed_on_disconnect() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (count_tx, count_rx) = mpsc::channel::<usize>();
    let (go_tx, go_rx) = mpsc::channel::<()>();

    let pub_handle = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut publisher = PubSocket::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(publisher.local_addr().unwrap()).unwrap();
            publisher.accept_subscriber().await.unwrap();

            // Wait until the subscriber has disconnected, then publish so the
            // worker detects the dead peer and evicts its subscription state.
            go_rx.recv().unwrap();
            for _ in 0..10 {
                publisher
                    .send(vec![Bytes::from_static(b"topic"), Bytes::from_static(b"x")])
                    .await
                    .ok();
                thread::sleep(Duration::from_millis(10));
            }
            // DESIRED observable: no subscribers remain after the disconnect.
            count_tx.send(publisher.subscriber_count()).unwrap();
        });
    });

    let addr = addr_rx.recv().unwrap();
    {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let stream = monocoque::rt::TcpStream::connect(addr).await.unwrap();
            let mut sub = SubSocket::from_tcp_with_options(stream, opts()).await.unwrap();
            sub.subscribe(b"topic").await.unwrap();
            // Drop the subscriber to disconnect.
            drop(sub);
        });
    }
    go_tx.send(()).unwrap();

    let remaining = count_rx.recv().unwrap();
    pub_handle.join().unwrap();

    assert_eq!(
        remaining, 0,
        "PUB still reports {remaining} subscribers after the only SUB disconnected; \
         its subscription state for that peer leaked"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Invariant 3: per-subscriber distinct-prefix cap.
//
// A subscriber must not be able to register an unbounded number of distinct
// subscription prefixes (a memory-amplification vector).
//
// STATUS: CURRENTLY UNENFORCED and not cleanly observable through the public API.
// The publisher's `SubscriptionState`/union stores prefixes in an unbounded `Vec`
// (monocoque-zmtp/src/publisher.rs) with no per-subscriber cap. There is no
// public accessor exposing a peer's registered-prefix count, so this guard
// becomes a unit test on the subscription reader in the rework. `#[ignore]`d
// documented placeholder rather than a fake assertion.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "guards per-subscriber prefix cap; currently unenforced (unbounded prefix Vec) and not reachable through the public API, becomes a unit test in the rework"]
fn per_subscriber_prefix_count_is_capped() {
    // Placeholder: the reworked subscription reader must reject or bound a
    // subscriber that registers more than a cap of distinct prefixes. Not
    // assertable through the public PubSocket/SubSocket API today (no prefix-count
    // accessor), so this is intentionally left as a documented guard.
}

// ─────────────────────────────────────────────────────────────────────────────
// Invariant 4: reused-PeerKey epoch guard on Subscribe/Unsubscribe.
//
// If a peer key/slot is reused after a disconnect, a stale Subscribe/Unsubscribe
// from the old peer must not mutate the NEW peer's subscription state.
//
// STATUS: not reachable through the public API. Peer keys/slots and the ordering
// of a stale subscription message against a slot reuse are internal to the
// subscription reader and worker pool; there is no public API to force slot
// reuse and inject a stale control frame deterministically. Covered structurally
// today (each subscriber reader owns its own channel and is torn down on
// disconnect) and will be a targeted unit test with an explicit epoch/generation
// stamp in the rework. `#[ignore]`d documented placeholder.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "guards reused-PeerKey epoch on Subscribe/Unsubscribe; not reachable through the public API (internal slot reuse), covered structurally and becomes a unit test with an epoch stamp in the rework"]
fn reused_peer_key_rejects_stale_subscription() {
    // Placeholder: the reworked subscription state must stamp each peer slot with
    // an epoch/generation so a Subscribe/Unsubscribe carrying a stale epoch is
    // ignored after the slot is reused by a new peer. Cannot be driven through the
    // public API (no way to force deterministic slot reuse + stale control frame),
    // so this is a documented structural guard pending the unit test.
}
