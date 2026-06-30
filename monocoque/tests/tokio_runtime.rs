//! Smoke test for the tokio runtime backend.
//!
//! Drives a PUSH/PULL round trip end to end on a tokio current-thread runtime
//! wrapped in a `LocalSet`, mirroring compio's thread-per-core model. Runs only
//! when the crate is built with `--no-default-features --features
//! runtime-tokio,zmq`.

#![cfg(all(feature = "runtime-tokio", feature = "zmq"))]

use bytes::Bytes;
use monocoque::zmq::{PullSocket, PushSocket};

#[test]
fn push_pull_round_trip_on_tokio() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread tokio runtime");
    let local = tokio::task::LocalSet::new();

    let port = portpicker::pick_unused_port().expect("pick a free port");
    let addr = format!("127.0.0.1:{port}");

    local.block_on(&rt, async move {
        // PushSocket::bind accepts one connection inside the call, so the PULL
        // connect has to make progress concurrently. A single join on the
        // current-thread executor interleaves the two handshakes.
        let (push_res, pull_res) = futures::join!(
            PushSocket::bind(addr.clone()),
            PullSocket::connect(addr.clone())
        );

        let (_listener, mut push) = push_res.expect("PUSH bind + accept");
        let mut pull = pull_res.expect("PULL connect");

        push.send(vec![Bytes::from_static(b"hi from tokio")])
            .await
            .expect("send");

        let msg = pull
            .recv()
            .await
            .expect("recv io")
            .expect("a message, not a closed channel");

        assert_eq!(msg.len(), 1);
        assert_eq!(msg[0], Bytes::from_static(b"hi from tokio"));
    });
}
