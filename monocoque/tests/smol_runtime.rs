//! Smoke test for the smol runtime backend.
//!
//! Drives a PUSH/PULL round trip through `rt::LocalRuntime`, the
//! backend-agnostic entry point (a smol `LocalExecutor` here). Runs only when
//! built with `--no-default-features --features runtime-smol,zmq`.

#![cfg(all(feature = "runtime-smol", feature = "zmq"))]

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{PullSocket, PushSocket};

#[test]
fn push_pull_round_trip_on_smol() {
    let rt = LocalRuntime::new().expect("build smol local runtime");
    rt.block_on(async move {
        let mut push = PushSocket::new();
        let ep = push.bind("tcp://127.0.0.1:0").await.expect("PUSH bind");
        let mut pull = PullSocket::new();
        pull.connect(&ep).await.expect("PULL connect");
        push.send("hi from smol").await.expect("send");
        assert_eq!(pull.recv().await.expect("recv"), [b"hi from smol"]);
    });
}
