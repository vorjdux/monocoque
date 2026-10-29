//! Smoke test for the tokio runtime backend.
//!
//! Drives a PUSH/PULL round trip on a raw tokio current-thread runtime wrapped
//! in a `LocalSet`, mirroring compio's thread-per-core model. Runs only when
//! built with `--no-default-features --features runtime-tokio,zmq`.

#![cfg(all(feature = "runtime-tokio", feature = "zmq"))]

use monocoque::zmq::{PullSocket, PushSocket};

#[test]
fn push_pull_round_trip_on_tokio() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread tokio runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let mut push = PushSocket::new();
        let ep = push.bind("tcp://127.0.0.1:0").await.expect("PUSH bind");
        let mut pull = PullSocket::new();
        pull.connect(&ep).await.expect("PULL connect");
        push.send("hi from tokio").await.expect("send");
        assert_eq!(pull.recv().await.expect("recv"), [b"hi from tokio"]);
    });
}
