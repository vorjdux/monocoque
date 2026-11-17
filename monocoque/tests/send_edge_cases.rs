//! Sends that put nothing on the wire.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, Msg, RouterSocket, SocketOptions};
use std::time::{Duration, Instant};

fn opts() -> SocketOptions {
    SocketOptions::default()
        .with_recv_timeout(Duration::from_secs(5))
        .with_linger(Some(Duration::from_secs(20)))
}

/// A message with no frames encodes to nothing. It used to be counted as
/// queued all the same, so `close()` waited its whole linger for bytes that
/// were never going to be written.
#[test]
fn an_empty_message_does_not_hold_up_close() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut server = DealerSocket::with_options(opts());
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client = DealerSocket::with_options(opts());
        client.connect(&ep).await.unwrap();
        client.send(Msg::from("first")).await.unwrap();
        assert_eq!(server.recv().await.unwrap(), [b"first"]);

        client.send(Msg::new()).await.unwrap();
        let began = Instant::now();
        client.close().await.unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "close waited {:?} on a message that was never encoded",
            began.elapsed()
        );
    });
}

/// The same for a ROUTER message that is only its identity frame: the
/// identity picks the peer and nothing is left to send.
#[test]
fn a_router_message_that_is_only_an_identity_does_not_hold_up_close() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut router = RouterSocket::with_options(opts());
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer = DealerSocket::with_options(opts());
        dealer.connect(&ep).await.unwrap();
        dealer.send(Msg::from("hello")).await.unwrap();

        let mut request = router.recv_owned().await.unwrap();
        let identity = request.pop_front().expect("identity frame");
        router.send(Msg::from(identity)).await.unwrap();

        let began = Instant::now();
        router.close().await.unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "close waited {:?} on a message that was never encoded",
            began.elapsed()
        );
    });
}
