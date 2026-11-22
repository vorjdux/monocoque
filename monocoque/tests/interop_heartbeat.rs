//! Heartbeats against an idle libzmq peer.
//!
//! A PING may carry a time-to-live. libzmq closes the connection when it then
//! hears nothing more from the sender for that long. monocoque used to send
//! its heartbeat interval as the TTL, while pinging an idle peer only every
//! second interval (the peer's PONG counted as traffic), so an idle libzmq
//! peer dropped the connection over and over.

use monocoque::zmq::{DealerSocket, SocketEvent, SocketOptions};
use std::time::Duration;

/// Connect a heartbeating DEALER to an idle libzmq ROUTER and count how many
/// times the connection drops over two seconds.
fn disconnects_while_idle(options: SocketOptions) -> usize {
    let ctx = zmq::Context::new();
    let router = ctx.socket(zmq::ROUTER).unwrap();
    router.bind("tcp://127.0.0.1:*").unwrap();
    let endpoint = router.get_last_endpoint().unwrap().unwrap();

    monocoque::rt::LocalRuntime::new()
        .unwrap()
        .block_on(async move {
            let mut dealer = DealerSocket::with_options(options);
            let events = dealer.monitor();
            dealer.connect(&endpoint).await.unwrap();
            while dealer.peer_count() == 0 {
                monocoque::rt::sleep(Duration::from_millis(5)).await;
            }
            monocoque::rt::sleep(Duration::from_secs(2)).await;
            let dropped = events
                .try_iter()
                .filter(|e| matches!(e, SocketEvent::Disconnected(_)))
                .count();
            assert_eq!(dealer.peer_count(), 1, "the connection is up at the end");
            dropped
        })
}

#[test]
fn default_heartbeats_keep_an_idle_libzmq_peer_connected() {
    let options = SocketOptions::default().with_heartbeat_ivl(Duration::from_millis(200));
    assert_eq!(disconnects_while_idle(options), 0);
}

/// With a TTL above the interval a PING goes out every interval, so the
/// peer's timer is always restarted before it runs out.
#[test]
fn a_ttl_above_the_interval_keeps_an_idle_libzmq_peer_connected() {
    let options = SocketOptions::default()
        .with_heartbeat_ivl(Duration::from_millis(200))
        .with_heartbeat_ttl(Duration::from_millis(600));
    assert_eq!(disconnects_while_idle(options), 0);
}
