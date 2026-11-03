//! XPUB reads subscriptions eagerly, but the reports it keeps for `recv` stay
//! within its receive HWM. Past it the rest wait in the subscriber's queue
//! and arrive later, in order, with none lost.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{SocketOptions, SubSocket, XPubSocket};
use std::time::Duration;

const SUBS: usize = 1_000;

#[test]
fn xpub_reports_past_the_recv_hwm_all_arrive_in_order() {
    LocalRuntime::new().unwrap().block_on(async {
        let opts = SocketOptions::default()
            .with_recv_hwm(4)
            .with_recv_timeout(Duration::from_secs(10));
        let mut xpub = XPubSocket::with_options(opts);
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::new();
        sub.connect(&ep).await.unwrap();
        for i in 0..SUBS {
            sub.subscribe(format!("topic-{i:04}")).await.unwrap();
        }
        for i in 0..SUBS {
            let report = xpub.recv_owned().await.unwrap();
            let body = report.frames().next().unwrap().clone();
            assert_eq!(body[0], 1, "report {i} is not a subscribe");
            assert_eq!(&body[1..], format!("topic-{i:04}").as_bytes());
        }
    });
}
