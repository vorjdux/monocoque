//! XPUB applies every subscription as it arrives, however far behind the
//! application is on reading the reports. In manual mode, where the
//! application applies them, the reports held for `recv` stay within the
//! receive HWM and the rest arrive later, in order, with none lost.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{SocketOptions, SubSocket, XPubSocket, XSubSocket};
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

/// A subscriber toggling one topic leaves at most the HWM of reports, and
/// a subscription made after the flood still takes effect and is reported.
#[test]
fn toggling_a_topic_cannot_grow_the_reports() {
    const TOGGLES: usize = 100_000;
    LocalRuntime::new().unwrap().block_on(async {
        let mut xpub =
            XPubSocket::with_options(opts().with_recv_timeout(Duration::from_millis(200)));
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(opts());
        sub.connect(&ep).await.unwrap();
        for _ in 0..TOGGLES {
            sub.subscribe("toggled").await.unwrap();
            sub.unsubscribe("toggled").await.unwrap();
        }
        sub.subscribe("after").await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "the subscription after the flood was never applied"
            );
            xpub.send("after").await.unwrap();
            if monocoque::rt::timeout(Duration::from_millis(20), sub.recv_owned())
                .await
                .is_ok()
            {
                break;
            }
        }
        let mut reports = Vec::new();
        while let Ok(r) = xpub.recv_owned().await {
            reports.push(r.frames().next().unwrap().to_vec());
        }
        assert!(reports.len() <= 6, "{} reports kept", reports.len());
        assert!(
            reports.iter().any(|r| r == b"\x01after"),
            "the last subscription's report was lost"
        );
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

/// Time for an XPUB that never reads its reports to apply `TOGGLES` toggles of
/// one topic, from a subscriber already holding `held` unread prefixes.
fn toggle_time(held: usize) -> Duration {
    const TOGGLES: usize = 20_000;
    LocalRuntime::new().unwrap().block_on(async {
        let mut xpub = XPubSocket::with_options(opts());
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(opts());
        sub.connect(&ep).await.unwrap();
        for i in 0..held {
            sub.subscribe(format!("held-{i:05}")).await.unwrap();
        }
        // Wait for the held prefixes to be applied, so only the toggles are
        // timed.
        if held > 0 {
            let last = format!("held-{:05}", held - 1);
            loop {
                xpub.send(last.clone()).await.unwrap();
                if monocoque::rt::timeout(Duration::from_millis(20), sub.recv_owned())
                    .await
                    .is_ok()
                {
                    break;
                }
            }
        }
        let start = std::time::Instant::now();
        for _ in 0..TOGGLES {
            sub.subscribe("toggled").await.unwrap();
            sub.unsubscribe("toggled").await.unwrap();
        }
        sub.subscribe("after").await.unwrap();
        loop {
            xpub.send("after").await.unwrap();
            if monocoque::rt::timeout(Duration::from_millis(1), sub.recv_owned())
                .await
                .is_ok()
            {
                break;
            }
        }
        start.elapsed()
    })
}

/// A report past the HWM costs the same however many reports are already
/// waiting: each one scanned all of them, so a subscriber holding many
/// unread prefixes and toggling a topic cost quadratic CPU (21.6 us a report
/// at 9 000 prefixes, against 0.08 us with none).
#[test]
fn reports_past_the_hwm_cost_the_same_with_many_held() {
    let empty = toggle_time(0);
    let held = toggle_time(9_000);
    // Loose enough for a debug build, where holding the prefixes costs about
    // 6 to 7 times as much and varies, and still far under the scan's roughly
    // 280 times.
    assert!(
        held < empty * 20 + Duration::from_millis(200),
        "with 9 000 prefixes held the toggles took {held:?}, against {empty:?} with none"
    );
}

/// A data frame from an XSUB that arrives while reports are parked, with the
/// inbox below its HWM, waits behind them instead of being dropped.
#[test]
fn a_data_frame_behind_parked_reports_is_kept() {
    const REPORTS: usize = 10;
    LocalRuntime::new().unwrap().block_on(async {
        let mut xpub = XPubSocket::with_options(opts());
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut xsub = XSubSocket::with_options(opts());
        xsub.connect(&ep).await.unwrap();
        for i in 0..REPORTS {
            xsub.send(format!("\x01t-{i}")).await.unwrap();
        }
        // Wait for the last subscription: four reports fill the inbox and
        // the rest are parked.
        let last = format!("t-{}", REPORTS - 1);
        loop {
            xpub.send(last.clone()).await.unwrap();
            if monocoque::rt::timeout(Duration::from_millis(20), xsub.recv_owned())
                .await
                .is_ok()
            {
                break;
            }
        }
        // Read two: the inbox is below its HWM, reports still parked.
        for _ in 0..2 {
            xpub.recv_owned().await.unwrap();
        }
        xsub.send("data").await.unwrap();
        // Let the XPUB take the data frame in while the reports are still
        // parked: a receive takes queued messages before reading more.
        monocoque::rt::sleep(Duration::from_millis(50)).await;
        assert!(xpub.has_message());
        let mut rest = Vec::new();
        while let Ok(m) = xpub.recv_owned().await {
            rest.push(m.frames().next().unwrap().to_vec());
            if rest.last().is_some_and(|f| f == b"data") {
                break;
            }
        }
        assert_eq!(
            rest.last().map(Vec::as_slice),
            Some(&b"data"[..]),
            "the data frame was lost"
        );
        assert_eq!(
            rest.len(),
            REPORTS - 2 + 1,
            "every report comes before it: {rest:?}"
        );
        assert!(rest[..REPORTS - 2].iter().all(|r| r[0] == 1));
    });
}
