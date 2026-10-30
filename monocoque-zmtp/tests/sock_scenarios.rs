//! Behavior scenarios on the multiplexer sockets: high-water marks, fan-out,
//! subscription options, reconnection, linger, vectored writes, STREAM peers,
//! XPUB verbosity and a PAIR proxy.

use bytes::Bytes;
use monocoque_core::error::MonocoqueError;
use monocoque_core::msg::Msg;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{LocalRuntime, sleep};
use monocoque_zmtp::sock::kinds::*;
use monocoque_zmtp::sock::proxy::{NoCapture, proxy};
use monocoque_zmtp::sock::stream::StreamSocket;
use std::io::{Read, Write};
use std::time::Duration;

fn rt() -> LocalRuntime {
    LocalRuntime::new().unwrap()
}

fn timeout_opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

/// Publish `probe` until `want` subscribers receive it, so later messages are
/// not lost to the subscription still being in flight.
async fn wait_subscribers(publisher: &mut PubSocket, probe: &'static str, want: usize) {
    for _ in 0..1000 {
        if publisher.send(Msg::from(probe)).await.unwrap() >= want {
            return;
        }
        sleep(Duration::from_millis(5)).await;
    }
    panic!("subscribers never arrived");
}

/// Receive the next message whose first frame is not `probe`.
async fn recv_skipping(sub: &mut SubSocket, probe: &[u8]) -> Msg {
    loop {
        let m = sub.recv_owned().await.unwrap();
        if m.first() != Some(probe) {
            return m;
        }
    }
}

// ── high-water marks ─────────────────────────────────────────────────────

/// PUB never blocks on a subscriber that stops reading: past the HWM it drops.
#[test]
fn pub_drops_for_a_slow_subscriber_instead_of_blocking() {
    const MSGS: usize = 2_000;
    rt().block_on(async {
        let mut publisher = PubSocket::with_options(SocketOptions::default().with_send_hwm(4));
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_millis(300)),
        );
        sub.subscribe(b"").await.unwrap();
        sub.connect(&ep).await.unwrap();
        wait_subscribers(&mut publisher, "probe", 1).await;

        // 2000 x 64 KiB is far more than the kernel buffers hold, so with the
        // subscriber not reading, most of it must be dropped, not queued.
        let body = Bytes::from(vec![7u8; 64 * 1024]);
        let start = std::time::Instant::now();
        for _ in 0..MSGS {
            publisher
                .send(Msg::from([Bytes::from_static(b"t"), body.clone()]))
                .await
                .unwrap();
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "PUB must not block on a slow subscriber"
        );

        let mut received = 0usize;
        while let Ok(m) = sub.recv_owned().await {
            if m.first() == Some(&b"t"[..]) {
                received += 1;
            }
        }
        assert!(
            received < MSGS,
            "expected drops past the HWM, got all {MSGS}"
        );
    });
}

/// DEALER waits at its HWM when the peer stops reading, and `send_timeout`
/// bounds the wait.
#[test]
fn dealer_send_waits_at_hwm_until_send_timeout() {
    rt().block_on(async {
        let mut router = RouterSocket::with_options(SocketOptions::default().with_recv_hwm(1));
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer = DealerSocket::with_options(
            SocketOptions::default()
                .with_send_hwm(5)
                .with_send_timeout(Duration::from_millis(200)),
        );
        dealer.connect(&ep).await.unwrap();
        while router.peer_count() < 1 {
            sleep(Duration::from_millis(5)).await;
        }

        let body = Bytes::from(vec![1u8; 1 << 20]);
        let mut err = None;
        for _ in 0..1_000 {
            if let Err(e) = dealer.send(Msg::from(body.clone())).await {
                err = Some(e);
                break;
            }
        }
        let err = err.expect("a peer that never reads must eventually stall the DEALER");
        assert!(err.is_timeout(), "expected a send timeout, got {err:?}");
    });
}

/// ROUTER never blocks on a full peer: it drops, or reports `Again` under
/// `router_mandatory`.
#[test]
fn router_send_at_hwm_drops_or_reports_again_when_mandatory() {
    for mandatory in [false, true] {
        rt().block_on(async {
            let mut router = RouterSocket::with_options(
                SocketOptions::default()
                    .with_send_hwm(5)
                    .with_router_mandatory(mandatory)
                    .with_send_timeout(Duration::from_millis(200)),
            );
            let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
            let mut dealer = DealerSocket::with_options(
                SocketOptions::default()
                    .with_routing_id(Bytes::from_static(b"peer"))
                    .with_recv_hwm(1),
            );
            dealer.connect(&ep).await.unwrap();
            while router.peer_count() < 1 {
                sleep(Duration::from_millis(5)).await;
            }

            let body = Bytes::from(vec![2u8; 1 << 20]);
            let mut again = false;
            for _ in 0..1_000 {
                let r = router
                    .send(Msg::from([Bytes::from_static(b"peer"), body.clone()]))
                    .await;
                match r {
                    Ok(()) => {}
                    Err(MonocoqueError::Again) => {
                        again = true;
                        break;
                    }
                    Err(e) => panic!("unexpected error {e:?}"),
                }
            }
            assert_eq!(again, mandatory, "mandatory={mandatory}");
        });
    }
}

/// Messages a peer sent before disconnecting are still delivered by ROUTER,
/// with that peer's identity, after the disconnect is seen.
#[test]
fn router_delivers_what_a_departed_peer_sent() {
    rt().block_on(async {
        let mut router = RouterSocket::with_options(timeout_opts());
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer =
            DealerSocket::with_options(timeout_opts().with_routing_id(Bytes::from_static(b"gone")));
        dealer.connect(&ep).await.unwrap();
        for i in 0..50u8 {
            dealer.send(Msg::copy_from_slice(&[i])).await.unwrap();
        }
        dealer.close().await.unwrap();
        // Let the ROUTER read everything and see the EOF before anyone asks.
        for _ in 0..500 {
            if router.peer_count() == 0 {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(router.peer_count(), 0, "the DEALER's close must be seen");
        for i in 0..50u8 {
            assert_eq!(router.recv().await.unwrap(), [&b"gone"[..], &[i]]);
        }
    });
}

/// Several DEALERs pushing hard through a small HWM into one ROUTER all make
/// progress; nothing deadlocks.
#[test]
fn dealers_through_a_small_hwm_do_not_deadlock() {
    const CLIENTS: usize = 3;
    const MSGS: usize = 10_000;
    let (ep_tx, ep_rx) = std::sync::mpsc::channel::<String>();
    let server = std::thread::spawn(move || {
        rt().block_on(async move {
            let mut router = RouterSocket::with_options(timeout_opts());
            ep_tx
                .send(router.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            for _ in 0..CLIENTS * MSGS {
                router.recv().await.unwrap();
            }
        });
    });
    let ep = ep_rx.recv().unwrap();
    let clients: Vec<_> = (0..CLIENTS)
        .map(|_| {
            let ep = ep.clone();
            std::thread::spawn(move || {
                rt().block_on(async move {
                    let mut dealer =
                        DealerSocket::with_options(SocketOptions::default().with_send_hwm(100));
                    dealer.connect(&ep).await.unwrap();
                    for i in 0..MSGS {
                        dealer
                            .send(Msg::from([Bytes::new(), Bytes::from(i.to_string())]))
                            .await
                            .unwrap();
                    }
                    dealer.close().await.unwrap();
                });
            })
        })
        .collect();
    for c in clients {
        c.join().expect("client panicked");
    }
    server.join().expect("server panicked");
}

// ── PUB/SUB fan-out and options ──────────────────────────────────────────

/// Each subscriber of a distinct topic gets exactly its own messages.
#[test]
fn pub_fans_out_distinct_topics_to_their_subscribers() {
    const N: usize = 4;
    const MSGS: usize = 20;
    rt().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut subs = Vec::new();
        for i in 0..N {
            let mut sub = SubSocket::with_options(timeout_opts());
            sub.subscribe(format!("t{i}")).await.unwrap();
            sub.subscribe(b"probe").await.unwrap();
            sub.connect(&ep).await.unwrap();
            subs.push(sub);
        }
        wait_subscribers(&mut publisher, "probe", N).await;
        for i in 0..N {
            for j in 0..MSGS {
                publisher
                    .send(Msg::from([
                        Bytes::from(format!("t{i}")),
                        Bytes::from(j.to_string()),
                    ]))
                    .await
                    .unwrap();
            }
        }
        for (i, sub) in subs.iter_mut().enumerate() {
            let topic = format!("t{i}");
            for j in 0..MSGS {
                let m = recv_skipping(sub, b"probe").await;
                assert_eq!(m.get(0), Some(topic.as_bytes()), "subscriber {i}");
                assert_eq!(m.get(1), Some(j.to_string().as_bytes()), "subscriber {i}");
            }
        }
    });
}

/// A broad prefix sees every message under it; a narrow one only its own.
#[test]
fn pub_fans_out_overlapping_prefixes() {
    rt().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut broad = SubSocket::with_options(timeout_opts());
        broad.subscribe(b"weather").await.unwrap();
        broad.connect(&ep).await.unwrap();
        let mut narrow = SubSocket::with_options(timeout_opts());
        narrow.subscribe(b"weather.temp").await.unwrap();
        narrow.connect(&ep).await.unwrap();
        wait_subscribers(&mut publisher, "weather.temp.probe", 2).await;

        for i in 0..5u32 {
            publisher
                .send(Msg::from([
                    Bytes::from_static(b"weather.temp"),
                    Bytes::from(i.to_string()),
                ]))
                .await
                .unwrap();
        }
        for i in 0..3u32 {
            publisher
                .send(Msg::from([
                    Bytes::from_static(b"weather.hum"),
                    Bytes::from(i.to_string()),
                ]))
                .await
                .unwrap();
        }
        publisher.send(Msg::from("weather.done")).await.unwrap();

        let mut broad_count = 0;
        loop {
            let m = recv_skipping(&mut broad, b"weather.temp.probe").await;
            if m.first() == Some(&b"weather.done"[..]) {
                break;
            }
            broad_count += 1;
        }
        assert_eq!(broad_count, 8, "the broad prefix sees all eight");

        for _ in 0..5 {
            let m = recv_skipping(&mut narrow, b"weather.temp.probe").await;
            assert_eq!(m.first(), Some(&b"weather.temp"[..]));
        }
        narrow.update_options(|o| o.with_recv_timeout(Duration::from_millis(100)));
        assert!(
            narrow.recv().await.unwrap_err().is_timeout(),
            "the narrow prefix sees nothing else"
        );
    });
}

/// Subscriptions given in the options are applied without `subscribe()`.
#[test]
fn sub_options_subscriptions_are_applied() {
    rt().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(
            timeout_opts()
                .with_subscribe(Bytes::from_static(b"alerts."))
                .with_subscribe(Bytes::from_static(b"metrics.")),
        );
        sub.connect(&ep).await.unwrap();
        wait_subscribers(&mut publisher, "alerts.probe", 1).await;

        for topic in [
            "alerts.fire",
            "ignore.me",
            "metrics.cpu",
            "ignore.also",
            "alerts.critical",
        ] {
            publisher.send(Msg::from(topic)).await.unwrap();
        }
        let mut got = Vec::new();
        while got.len() < 3 {
            let m = sub.recv_owned().await.unwrap();
            if m.first() != Some(&b"alerts.probe"[..]) {
                got.push(m.first().unwrap().to_vec());
            }
        }
        assert_eq!(
            got,
            vec![
                b"alerts.fire".to_vec(),
                b"metrics.cpu".to_vec(),
                b"alerts.critical".to_vec()
            ]
        );
    });
}

/// `invert_matching` on both ends delivers what does not match.
#[test]
fn invert_matching_delivers_non_matching_messages() {
    rt().block_on(async {
        let mut publisher =
            PubSocket::with_options(SocketOptions::default().with_invert_matching(true));
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(
            timeout_opts()
                .with_invert_matching(true)
                .with_subscribe(Bytes::from_static(b"news.")),
        );
        sub.connect(&ep).await.unwrap();
        wait_subscribers(&mut publisher, "probe", 1).await;

        publisher.send(Msg::from("news.breaking")).await.unwrap();
        publisher.send(Msg::from("weather.today")).await.unwrap();
        let m = recv_skipping(&mut sub, b"probe").await;
        assert_eq!(m.first(), Some(&b"weather.today"[..]));
    });
}

/// A burst published back to back, within the HWM, arrives complete and in
/// order.
#[test]
fn pub_burst_arrives_complete_and_in_order() {
    const MSGS: u32 = 500;
    rt().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(timeout_opts());
        sub.subscribe(b"").await.unwrap();
        sub.connect(&ep).await.unwrap();
        wait_subscribers(&mut publisher, "probe", 1).await;
        for i in 0..MSGS {
            publisher
                .send(Msg::copy_from_slice(&i.to_be_bytes()))
                .await
                .unwrap();
        }
        for i in 0..MSGS {
            let m = recv_skipping(&mut sub, b"probe").await;
            assert_eq!(m.first(), Some(&i.to_be_bytes()[..]));
        }
    });
}

/// Without `xpub_verbose` a second subscriber to a known prefix is not
/// reported; with it, every subscription is.
#[test]
fn xpub_verbose_reports_repeated_subscriptions() {
    for verbose in [false, true] {
        rt().block_on(async {
            let mut xpub = XPubSocket::with_options(
                SocketOptions::default()
                    .with_xpub_verbose(verbose)
                    .with_recv_timeout(Duration::from_millis(300)),
            );
            let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
            let mut a = SubSocket::new();
            a.subscribe(b"topic").await.unwrap();
            a.connect(&ep).await.unwrap();
            let first = xpub.recv_subscription().await.unwrap();
            assert!(first.is_subscribe());

            let mut b = SubSocket::new();
            b.subscribe(b"topic").await.unwrap();
            b.connect(&ep).await.unwrap();
            let second = xpub.recv_subscription().await;
            if verbose {
                let ev = second.unwrap();
                assert!(ev.is_subscribe());
                assert_eq!(&ev.prefix()[..], b"topic");
            } else {
                assert!(second.unwrap_err().is_timeout());
            }
        });
    }
}

// ── PUSH/PULL ────────────────────────────────────────────────────────────

/// PULL can bind and PUSH connect, the reverse of the usual topology.
#[test]
fn pull_binds_push_connects() {
    rt().block_on(async {
        let mut pull = PullSocket::with_options(timeout_opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        for i in 0..5u8 {
            push.send(Msg::copy_from_slice(&[i])).await.unwrap();
        }
        for i in 0..5u8 {
            assert_eq!(pull.recv().await.unwrap(), [[i]]);
        }
    });
}

/// `close()` with a linger delivers what is still queued.
#[test]
fn push_close_flushes_queued_messages_within_linger() {
    const MSGS: usize = 200;
    let (ep_tx, ep_rx) = std::sync::mpsc::channel::<String>();
    let puller = std::thread::spawn(move || {
        rt().block_on(async move {
            let mut pull = PullSocket::with_options(timeout_opts());
            ep_tx
                .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            for i in 0..MSGS {
                let m = pull.recv_owned().await.unwrap();
                assert_eq!(m.get(0), Some(i.to_string().as_bytes()));
                assert_eq!(m.byte_len(), i.to_string().len() + 32 * 1024);
            }
        });
    });
    let ep = ep_rx.recv().unwrap();
    rt().block_on(async {
        let mut push = PushSocket::with_options(
            SocketOptions::default().with_linger(Some(Duration::from_secs(5))),
        );
        push.connect(&ep).await.unwrap();
        let body = Bytes::from(vec![3u8; 32 * 1024]);
        for i in 0..MSGS {
            push.send(Msg::from([Bytes::from(i.to_string()), body.clone()]))
                .await
                .unwrap();
        }
        push.close().await.unwrap();
    });
    puller.join().expect("every queued message must arrive");
}

/// Above the vectored threshold, a body goes out as its own iovec; what
/// arrives is byte-identical, alone or mixed with small frames.
#[test]
fn vectored_large_frames_arrive_byte_identical() {
    rt().block_on(async {
        let mut pull = PullSocket::with_options(timeout_opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push =
            PushSocket::with_options(SocketOptions::default().with_vectored_write_threshold(256));
        push.connect(&ep).await.unwrap();
        let big = Bytes::from((0..100_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>());
        let small = Bytes::from_static(b"head");
        push.send(Msg::from(big.clone())).await.unwrap();
        push.send(Msg::from([small.clone(), big.clone()]))
            .await
            .unwrap();
        assert_eq!(pull.recv_owned().await.unwrap(), [&big[..]]);
        assert_eq!(pull.recv_owned().await.unwrap(), [&small[..], &big[..]]);
    });
}

// ── reconnection ─────────────────────────────────────────────────────────

/// A DEALER redials a ROUTER that went away and came back on the same port;
/// traffic flows both ways again.
#[test]
fn dealer_reconnects_after_the_router_restarts() {
    rt().block_on(async {
        let mut router = RouterSocket::with_options(timeout_opts());
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer = DealerSocket::with_options(
            timeout_opts()
                .with_routing_id(Bytes::from_static(b"d"))
                .with_reconnect_ivl(Duration::from_millis(10))
                .with_reconnect_ivl_max(Duration::from_millis(100)),
        );
        dealer.connect(&ep).await.unwrap();
        dealer.send(Msg::from("first")).await.unwrap();
        assert_eq!(router.recv().await.unwrap(), [&b"d"[..], b"first"]);
        router.close().await.unwrap();
        sleep(Duration::from_millis(50)).await;

        let mut router = RouterSocket::with_options(timeout_opts());
        router.bind(&ep).await.unwrap();
        dealer.send(Msg::from("second")).await.unwrap();
        assert_eq!(router.recv().await.unwrap(), [&b"d"[..], b"second"]);
        router
            .send(Msg::from([
                Bytes::from_static(b"d"),
                Bytes::from_static(b"back"),
            ]))
            .await
            .unwrap();
        assert_eq!(dealer.recv().await.unwrap(), [b"back"]);
    });
}

// ── STREAM ───────────────────────────────────────────────────────────────

fn tcp_addr(ep: &str) -> &str {
    ep.trim_start_matches("tcp://")
}

/// Every raw peer gets its own identity, and a send reaches only its target.
#[test]
fn stream_peers_get_distinct_identities_and_selective_sends() {
    const N: usize = 3;
    rt().block_on(async {
        let mut server = StreamSocket::with_options(timeout_opts());
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut clients: Vec<_> = (0..N)
            .map(|_| {
                let c = std::net::TcpStream::connect(tcp_addr(&ep)).unwrap();
                c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                c
            })
            .collect();
        for (i, c) in clients.iter_mut().enumerate() {
            c.write_all(format!("hello {i}").as_bytes()).unwrap();
        }
        let mut ids = std::collections::HashMap::new();
        while ids.len() < N {
            let m = server.recv_owned().await.unwrap();
            let data = m.get(1).unwrap();
            if !data.is_empty() {
                ids.insert(data.to_vec(), m.frame_bytes(0).unwrap());
            }
        }
        let unique: std::collections::HashSet<_> = ids.values().collect();
        assert_eq!(unique.len(), N, "identities must be unique");

        let target = ids[&b"hello 1"[..]].clone();
        server
            .send(Msg::from([target, Bytes::from_static(b"only you")]))
            .await
            .unwrap();
        server.flush().await.unwrap();
        let mut buf = [0u8; 16];
        let n = clients[1].read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"only you");
        clients[0]
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert!(
            clients[0].read(&mut buf).is_err(),
            "other peers get nothing"
        );

        let unknown = server
            .send(Msg::from([
                Bytes::from_static(b"nobody"),
                Bytes::from_static(b"x"),
            ]))
            .await;
        assert!(matches!(unknown, Err(MonocoqueError::HostUnreachable)));
    });
}

/// A STREAM peer that stops reading fills its queue; past the HWM a send
/// fails with `Again` instead of buffering without bound.
#[test]
fn stream_send_respects_send_hwm() {
    rt().block_on(async {
        let mut server = StreamSocket::with_options(timeout_opts().with_send_hwm(5));
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let _client = std::net::TcpStream::connect(tcp_addr(&ep)).unwrap();
        let hello = server.recv_owned().await.unwrap();
        let id = hello.frame_bytes(0).unwrap();

        let body = Bytes::from(vec![4u8; 1 << 20]);
        let mut again = false;
        for _ in 0..1_000 {
            match server.send(Msg::from([id.clone(), body.clone()])).await {
                Ok(()) => sleep(Duration::from_millis(1)).await,
                Err(MonocoqueError::Again) => {
                    again = true;
                    break;
                }
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
        assert!(again, "the queue must be bounded by send_hwm");
    });
}

/// Once the server closes a peer, data that peer sends later is not read.
#[test]
fn stream_closed_peer_is_no_longer_read() {
    rt().block_on(async {
        let mut server = StreamSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_millis(300)),
        );
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client = std::net::TcpStream::connect(tcp_addr(&ep)).unwrap();
        let hello = server.recv_owned().await.unwrap();
        let id = hello.frame_bytes(0).unwrap();

        server
            .send(Msg::from([id.clone(), Bytes::new()]))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        assert_eq!(server.peer_count(), 0);
        let _ = client.write_all(b"after-close-data");

        while let Ok(m) = server.recv_owned().await {
            assert_ne!(
                m.get(1),
                Some(&b"after-close-data"[..]),
                "a closed peer must not be read"
            );
        }
    });
}

// ── proxy ────────────────────────────────────────────────────────────────

/// A proxy between two PAIR sockets forwards in both directions.
#[test]
fn proxy_between_pairs_forwards_both_ways() {
    rt().block_on(async {
        let mut front = PairSocket::new();
        let front_ep = front.bind("inproc://scenario-proxy-front").await.unwrap();
        let mut back = PairSocket::new();
        let back_ep = back.bind("inproc://scenario-proxy-back").await.unwrap();
        let mut a = PairSocket::with_options(timeout_opts());
        a.connect(&front_ep).await.unwrap();
        let mut b = PairSocket::with_options(timeout_opts());
        b.connect(&back_ep).await.unwrap();

        let task = monocoque_core::rt::spawn(async move {
            let _ = proxy(&mut front, &mut back, None::<&mut NoCapture>).await;
        });

        a.send(Msg::from("A to B")).await.unwrap();
        assert_eq!(b.recv().await.unwrap(), [b"A to B"]);
        b.send(Msg::from("B to A")).await.unwrap();
        assert_eq!(a.recv().await.unwrap(), [b"B to A"]);
        drop(task);
    });
}
