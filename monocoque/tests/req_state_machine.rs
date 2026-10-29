//! REQ socket state machine tests.
//!
//! Covers strict alternation, relaxed mode (a new request abandons the
//! pending one) and request correlation.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{MonocoqueError, RepSocket, ReqSocket, SocketOptions};

/// Bind a REP that echoes `count` requests back with a `reply:` prefix on
/// the first frame, running on the local executor.
async fn echo_rep(count: usize) -> String {
    let mut rep = RepSocket::new();
    let ep = rep.bind("tcp://127.0.0.1:0").await.unwrap();
    monocoque::rt::spawn_detached(async move {
        for _ in 0..count {
            let mut msg = rep.recv_owned().await.unwrap();
            let body = msg.pop_front().unwrap();
            let mut reply = b"reply:".to_vec();
            reply.extend_from_slice(&body);
            rep.send(reply).await.unwrap();
        }
        let _ = rep.close().await;
    });
    ep
}

#[test]
fn strict_send_send_fails() {
    LocalRuntime::new().unwrap().block_on(async {
        let ep = echo_rep(1).await;
        let mut req = ReqSocket::new();
        req.connect(&ep).await.unwrap();
        req.send("request1").await.unwrap();
        assert!(matches!(
            req.send("request2").await,
            Err(MonocoqueError::InvalidState(_))
        ));
        assert_eq!(req.recv().await.unwrap(), [b"reply:request1"]);
    });
}

#[test]
fn strict_recv_before_send_fails() {
    LocalRuntime::new().unwrap().block_on(async {
        let ep = echo_rep(0).await;
        let mut req = ReqSocket::new();
        req.connect(&ep).await.unwrap();
        assert!(matches!(
            req.recv().await,
            Err(MonocoqueError::InvalidState(_))
        ));
        // A reply has been consumed: a second recv is again out of order.
        let ep = echo_rep(1).await;
        let mut req = ReqSocket::new();
        req.connect(&ep).await.unwrap();
        req.send("x").await.unwrap();
        req.recv().await.unwrap();
        assert!(matches!(
            req.recv().await,
            Err(MonocoqueError::InvalidState(_))
        ));
    });
}

#[test]
fn strict_alternation_works() {
    LocalRuntime::new().unwrap().block_on(async {
        let ep = echo_rep(3).await;
        let mut req = ReqSocket::new();
        req.connect(&ep).await.unwrap();
        for i in 0..3 {
            req.send(format!("request{i}")).await.unwrap();
            let expected = format!("reply:request{i}");
            assert_eq!(req.recv().await.unwrap(), [expected.as_bytes()]);
        }
    });
}

/// Relaxed plus correlated: a second request abandons the first, and the
/// stale reply to the first is discarded instead of being returned.
#[test]
fn relaxed_new_request_abandons_pending_one() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut rep = RepSocket::new();
        let ep = rep.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut req = ReqSocket::with_options(
            SocketOptions::default()
                .with_req_relaxed(true)
                .with_req_correlate(true),
        );
        req.connect(&ep).await.unwrap();
        req.send("request1").await.unwrap();
        req.send("request2").await.unwrap();
        for _ in 0..2 {
            let msg = rep.recv_owned().await.unwrap();
            rep.send(msg).await.unwrap();
        }
        assert_eq!(req.recv().await.unwrap(), [b"request2"]);
    });
}

/// Correlation ids travel in front of the envelope and are stripped again.
#[test]
fn correlation_id_is_added_and_stripped() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut rep = RepSocket::new();
        let ep = rep.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut req =
            ReqSocket::with_options(SocketOptions::default().with_req_correlate(true));
        req.connect(&ep).await.unwrap();
        req.send("payload").await.unwrap();
        let msg = rep.recv_owned().await.unwrap();
        assert_eq!(msg, [b"payload"]);
        rep.send(msg).await.unwrap();
        assert_eq!(req.recv().await.unwrap(), [b"payload"]);
    });
}
