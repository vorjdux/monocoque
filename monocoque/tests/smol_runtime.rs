//! Smoke test for the smol runtime backend.
//!
//! Drives a PUSH/PULL round trip end to end through `rt::LocalRuntime`, the
//! backend-agnostic entry point (a smol `LocalExecutor` here). Runs only when
//! the crate is built with `--no-default-features --features
//! runtime-smol,zmq`.

#![cfg(all(feature = "runtime-smol", feature = "zmq"))]

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{PullSocket, PushSocket};

#[test]
fn push_pull_round_trip_on_smol() {
    let rt = LocalRuntime::new().expect("build smol local runtime");

    let port = portpicker::pick_unused_port().expect("pick a free port");
    let addr = format!("127.0.0.1:{port}");

    rt.block_on(async move {
        // PushSocket::bind accepts one connection inside the call, so the PULL
        // connect has to make progress concurrently. A single join on the
        // single-threaded executor interleaves the two handshakes.
        let (push_res, pull_res) = futures::join!(
            PushSocket::bind(addr.clone()),
            PullSocket::connect(addr.clone())
        );

        let (_listener, mut push) = push_res.expect("PUSH bind + accept");
        let mut pull = pull_res.expect("PULL connect");

        push.send(vec![Bytes::from_static(b"hi from smol")])
            .await
            .expect("send");

        let msg = pull
            .recv()
            .await
            .expect("recv io")
            .expect("a message, not a closed channel");

        assert_eq!(msg.len(), 1);
        assert_eq!(msg[0], Bytes::from_static(b"hi from smol"));
    });
}
