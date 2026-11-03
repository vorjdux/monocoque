//! XPUB applies every subscription as it arrives, however far behind the
//! application is on reading the reports. In manual mode, where the
//! application applies them, the reports held for `recv` stay within the
//! receive HWM and the rest arrive later, in order, with none lost.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{SocketOptions, SubSocket, XPubSocket};
use std::time::Duration;

fn opts() -> SocketOptions {
    SocketOptions::default()
        .with_recv_hwm(4)
        .with_recv_timeout(Duration::from_secs(3))
}

#[test]
fn xpub_applies_subscriptions_past_the_recv_hwm_without_a_recv() {
    const SUBS: usize = 1_500;
    LocalRuntime::new().unwrap().block_on(async {
        let mut xpub = XPubSocket::with_options(opts());
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(opts());
        sub.connect(&ep).await.unwrap();
        for i in 0..SUBS {
            sub.subscribe(format!("topic-{i:04}")).await.unwrap();
        }
        let last = format!("topic-{:04}", SUBS - 1);
        // Publish until the last subscription has been applied; the reports
        // are never received.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let got = loop {
            assert!(
                std::time::Instant::now() < deadline,
                "the last subscription was never applied"
            );
            xpub.send(last.clone()).await.unwrap();
            match monocoque::rt::timeout(Duration::from_millis(20), sub.recv_owned()).await {
                Ok(msg) => break msg.unwrap(),
                Err(_) => continue,
            }
        };
        assert_eq!(got.frames().next().unwrap(), last.as_bytes());
    });
}

#[test]
fn manual_xpub_reports_past_the_recv_hwm_all_arrive_in_order() {
    const SUBS: usize = 1_000;
    LocalRuntime::new().unwrap().block_on(async {
        let mut xpub = XPubSocket::with_options(opts().with_xpub_manual(true));
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::new();
        sub.connect(&ep).await.unwrap();
        for i in 0..SUBS {
            sub.subscribe(format!("topic-{i:04}")).await.unwrap();
        }
        for i in 0..SUBS {
            let report = xpub.recv_owned().await.unwrap();
            let body = report.frames().next().unwrap();
            assert_eq!(body[0], 1, "report {i} is not a subscribe");
            assert_eq!(&body[1..], format!("topic-{i:04}").as_bytes());
        }
    });
}
