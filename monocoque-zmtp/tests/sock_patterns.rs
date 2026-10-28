//! Every socket pattern over TCP, IPC and inproc, and against libzmq.

use bytes::Bytes;
use monocoque_core::error::MonocoqueError;
use monocoque_core::msg::Msg;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{LocalRuntime, sleep};
use monocoque_zmtp::sock::kinds::*;
use std::time::Duration;

fn rt() -> LocalRuntime {
    LocalRuntime::new().unwrap()
}

fn timeout_opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

fn transports(tag: &str) -> Vec<String> {
    let ipc = std::env::temp_dir().join(format!("mq-sock-{tag}-{}.sock", std::process::id()));
    vec![
        "tcp://127.0.0.1:0".to_string(),
        format!("ipc://{}", ipc.display()),
        format!("inproc://sock-{tag}"),
    ]
}

#[test]
fn req_rep_round_trips_on_every_transport() {
    for ep in transports("reqrep") {
        rt().block_on(async {
            let mut rep = RepSocket::with_options(timeout_opts());
            let bound = rep.bind(&ep).await.unwrap();
            let mut req = ReqSocket::with_options(timeout_opts());
            req.connect(&bound).await.unwrap();
            for i in 0..50u32 {
                let body = format!("req {i}");
                req.send(Msg::from(body.clone())).await.unwrap();
                let got = rep.recv().await.unwrap();
                assert_eq!(got, [body.as_bytes()], "on {ep}");
                rep.send(Msg::from(format!("rep {i}"))).await.unwrap();
                let reply = req.recv().await.unwrap();
                assert_eq!(reply, [format!("rep {i}").as_bytes()], "on {ep}");
            }
        });
    }
}

#[test]
fn req_and_rep_enforce_alternation() {
    rt().block_on(async {
        let mut rep = RepSocket::with_options(timeout_opts());
        let ep = rep.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut req = ReqSocket::with_options(timeout_opts());
        req.connect(&ep).await.unwrap();
        assert!(matches!(
            req.recv().await,
            Err(MonocoqueError::InvalidState(_))
        ));
        assert!(matches!(
            rep.send(Msg::from("x")).await,
            Err(MonocoqueError::InvalidState(_))
        ));
        req.send(Msg::from("a")).await.unwrap();
        assert!(matches!(
            req.send(Msg::from("b")).await,
            Err(MonocoqueError::InvalidState(_))
        ));
        rep.recv().await.unwrap();
        assert!(matches!(
            rep.recv().await,
            Err(MonocoqueError::InvalidState(_))
        ));
        rep.send(Msg::from("r")).await.unwrap();
        assert_eq!(req.recv().await.unwrap(), [b"r"]);
    });
}

#[test]
fn dealer_router_routes_by_identity() {
    for ep in transports("dr") {
        rt().block_on(async {
            let mut router = RouterSocket::with_options(timeout_opts());
            let bound = router.bind(&ep).await.unwrap();
            let mut a = DealerSocket::with_options(
                timeout_opts().with_routing_id(Bytes::from_static(b"alice")),
            );
            let mut b = DealerSocket::with_options(
                timeout_opts().with_routing_id(Bytes::from_static(b"bob")),
            );
            a.connect(&bound).await.unwrap();
            b.connect(&bound).await.unwrap();
            a.send(Msg::from("from a")).await.unwrap();
            b.send(Msg::from("from b")).await.unwrap();
            let mut seen = Vec::new();
            for _ in 0..2 {
                let m = router.recv_owned().await.unwrap();
                seen.push((m.get(0).unwrap().to_vec(), m.get(1).unwrap().to_vec()));
            }
            seen.sort();
            assert_eq!(
                seen,
                vec![
                    (b"alice".to_vec(), b"from a".to_vec()),
                    (b"bob".to_vec(), b"from b".to_vec())
                ],
                "on {ep}"
            );
            router
                .send(Msg::from([
                    Bytes::from_static(b"bob"),
                    Bytes::from_static(b"to b"),
                ]))
                .await
                .unwrap();
            router
                .send(Msg::from([
                    Bytes::from_static(b"alice"),
                    Bytes::from_static(b"to a"),
                ]))
                .await
                .unwrap();
            assert_eq!(b.recv().await.unwrap(), [b"to b"]);
            assert_eq!(a.recv().await.unwrap(), [b"to a"]);
        });
    }
}

#[test]
fn router_mandatory_reports_unknown_peers() {
    rt().block_on(async {
        let mut router = RouterSocket::with_options(timeout_opts().with_router_mandatory(true));
        router.bind("tcp://127.0.0.1:0").await.unwrap();
        let r = router
            .send(Msg::from([
                Bytes::from_static(b"nobody"),
                Bytes::from_static(b"x"),
            ]))
            .await;
        assert!(matches!(r, Err(MonocoqueError::HostUnreachable)));
        let mut quiet = RouterSocket::new();
        quiet.bind("tcp://127.0.0.1:0").await.unwrap();
        quiet
            .send(Msg::from([
                Bytes::from_static(b"nobody"),
                Bytes::from_static(b"x"),
            ]))
            .await
            .unwrap();
    });
}

#[test]
fn push_pull_load_balances_across_workers() {
    rt().block_on(async {
        let mut push = PushSocket::new();
        let ep = push.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut w1 = PullSocket::with_options(timeout_opts());
        let mut w2 = PullSocket::with_options(timeout_opts());
        w1.connect(&ep).await.unwrap();
        w2.connect(&ep).await.unwrap();
        while push.peer_count() < 2 {
            sleep(Duration::from_millis(5)).await;
        }
        for i in 0..10u8 {
            push.send(Msg::copy_from_slice(&[i])).await.unwrap();
        }
        let mut got = Vec::new();
        for _ in 0..5 {
            got.push(w1.recv_owned().await.unwrap().first().unwrap()[0]);
            got.push(w2.recv_owned().await.unwrap().first().unwrap()[0]);
        }
        got.sort_unstable();
        assert_eq!(got, (0..10).collect::<Vec<u8>>());
    });
}

#[test]
fn pub_sub_filters_by_prefix() {
    for ep in transports("ps") {
        rt().block_on(async {
            let mut publisher = PubSocket::new();
            let bound = publisher.bind(&ep).await.unwrap();
            let mut weather = SubSocket::with_options(timeout_opts());
            let mut all = SubSocket::with_options(timeout_opts());
            weather.subscribe(b"weather.").await.unwrap();
            all.subscribe(b"").await.unwrap();
            weather.connect(&bound).await.unwrap();
            all.connect(&bound).await.unwrap();
            // Publish until both subscriptions have landed.
            loop {
                let n = publisher.send(Msg::from("weather.probe")).await.unwrap();
                if n == 2 {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
            publisher
                .send(Msg::from([
                    Bytes::from_static(b"stocks.x"),
                    Bytes::from_static(b"1"),
                ]))
                .await
                .unwrap();
            publisher
                .send(Msg::from([
                    Bytes::from_static(b"weather.london"),
                    Bytes::from_static(b"sunny"),
                ]))
                .await
                .unwrap();
            let mut w = weather.recv_owned().await.unwrap();
            while w.first() == Some(&b"weather.probe"[..]) {
                w = weather.recv_owned().await.unwrap();
            }
            assert_eq!(w, [&b"weather.london"[..], b"sunny"], "on {ep}");
            let mut seen = Vec::new();
            while seen.len() < 2 {
                let m = all.recv_owned().await.unwrap();
                if m.first() != Some(&b"weather.probe"[..]) {
                    seen.push(m.first().unwrap().to_vec());
                }
            }
            assert_eq!(seen, vec![b"stocks.x".to_vec(), b"weather.london".to_vec()]);
        });
    }
}

#[test]
fn xpub_reports_subscriptions_and_xsub_forwards_them() {
    rt().block_on(async {
        let mut xpub = XPubSocket::with_options(timeout_opts());
        let ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut xsub = XSubSocket::with_options(timeout_opts());
        xsub.connect(&ep).await.unwrap();
        xsub.subscribe(b"t").await.unwrap();
        let ev = xpub.recv_subscription().await.unwrap();
        assert!(ev.is_subscribe());
        assert_eq!(&ev.prefix()[..], b"t");
        xpub.send(Msg::from("t1")).await.unwrap();
        assert_eq!(xsub.recv().await.unwrap(), [b"t1"]);
        xsub.unsubscribe(b"t").await.unwrap();
        let ev = xpub.recv_subscription().await.unwrap();
        assert!(ev.is_unsubscribe());
    });
}

#[test]
fn pair_refuses_a_second_peer() {
    rt().block_on(async {
        let mut a = PairSocket::with_options(timeout_opts());
        let ep = a.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut b = PairSocket::with_options(timeout_opts());
        b.connect(&ep).await.unwrap();
        b.send(Msg::from("hi")).await.unwrap();
        assert_eq!(a.recv().await.unwrap(), [b"hi"]);
        let mut c = PairSocket::new();
        c.connect(&ep).await.unwrap();
        sleep(Duration::from_millis(100)).await;
        assert_eq!(a.peer_count(), 1);
        a.send(Msg::from("back")).await.unwrap();
        assert_eq!(b.recv().await.unwrap(), [b"back"]);
    });
}

#[test]
fn recv_timeout_elapses_without_losing_later_messages() {
    rt().block_on(async {
        let mut pull = PullSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_millis(30)),
        );
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        assert!(matches!(pull.recv().await, Err(MonocoqueError::TimedOut)));
        push.send(Msg::from("after")).await.unwrap();
        assert_eq!(pull.recv().await.unwrap(), [b"after"]);
        // Non-blocking mode reports Again instead of waiting.
        pull.update_options(|o| o.with_recv_timeout(Duration::ZERO));
        assert!(matches!(pull.recv().await, Err(MonocoqueError::Again)));
    });
}

// ── libzmq interop ───────────────────────────────────────────────────────

fn zmq_ctx() -> zmq::Context {
    zmq::Context::new()
}

#[test]
fn interop_req_to_libzmq_rep() {
    let ctx = zmq_ctx();
    let rep = ctx.socket(zmq::REP).unwrap();
    rep.bind("tcp://127.0.0.1:*").unwrap();
    let ep = rep.get_last_endpoint().unwrap().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..20 {
            let parts = rep.recv_multipart(0).unwrap();
            rep.send_multipart(parts, 0).unwrap();
        }
    });
    rt().block_on(async {
        let mut req = ReqSocket::with_options(timeout_opts());
        req.connect(&ep).await.unwrap();
        for i in 0..20u32 {
            let msg: Msg = [
                Bytes::from(format!("part1-{i}")),
                Bytes::from(vec![7u8; 1000 + i as usize]),
            ]
            .into();
            req.send(msg.clone()).await.unwrap();
            assert_eq!(req.recv_owned().await.unwrap(), msg);
        }
    });
    server.join().unwrap();
}

#[test]
fn interop_libzmq_dealer_to_router() {
    let ctx = zmq_ctx();
    rt().block_on(async {
        let mut router = RouterSocket::with_options(timeout_opts());
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let client = std::thread::spawn({
            let ctx = ctx.clone();
            move || {
                let dealer = ctx.socket(zmq::DEALER).unwrap();
                dealer.set_identity(b"libzmq-dealer").unwrap();
                dealer.connect(&ep).unwrap();
                dealer.send("hello", 0).unwrap();
                let reply = dealer.recv_multipart(0).unwrap();
                assert_eq!(reply, vec![b"world".to_vec()]);
            }
        });
        let m = router.recv_owned().await.unwrap();
        assert_eq!(m, [&b"libzmq-dealer"[..], b"hello"]);
        router
            .send(Msg::from([
                Bytes::from_static(b"libzmq-dealer"),
                Bytes::from_static(b"world"),
            ]))
            .await
            .unwrap();
        router.flush().await.unwrap();
        // The client thread blocks this runtime's thread only after it is done.
        while !client.is_finished() {
            sleep(Duration::from_millis(5)).await;
        }
        client.join().unwrap();
    });
}

#[test]
fn interop_libzmq_pub_to_sub() {
    let ctx = zmq_ctx();
    let publisher = ctx.socket(zmq::PUB).unwrap();
    publisher.bind("tcp://127.0.0.1:*").unwrap();
    let ep = publisher.get_last_endpoint().unwrap().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pub_thread = std::thread::spawn({
        let stop = std::sync::Arc::clone(&stop);
        move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                publisher.send_multipart(["news", "x"], 0).unwrap();
                publisher.send_multipart(["other", "y"], 0).unwrap();
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    });
    rt().block_on(async {
        let mut sub = SubSocket::with_options(timeout_opts());
        sub.subscribe(b"news").await.unwrap();
        sub.connect(&ep).await.unwrap();
        for _ in 0..5 {
            assert_eq!(sub.recv().await.unwrap(), [&b"news"[..], b"x"]);
        }
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    pub_thread.join().unwrap();
}

#[test]
fn interop_pub_to_libzmq_sub() {
    let ctx = zmq_ctx();
    rt().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let sub_thread = std::thread::spawn({
            let ctx = ctx.clone();
            move || {
                let sub = ctx.socket(zmq::SUB).unwrap();
                sub.set_subscribe(b"a").unwrap();
                sub.set_rcvtimeo(5000).unwrap();
                sub.connect(&ep).unwrap();
                let m = sub.recv_multipart(0).unwrap();
                assert_eq!(m[0], b"a1");
            }
        });
        while !sub_thread.is_finished() {
            publisher.send(Msg::from("b1")).await.unwrap();
            publisher.send(Msg::from("a1")).await.unwrap();
            sleep(Duration::from_millis(5)).await;
        }
        sub_thread.join().unwrap();
    });
}

#[test]
fn interop_push_to_libzmq_pull_with_large_frames() {
    let ctx = zmq_ctx();
    let pull = ctx.socket(zmq::PULL).unwrap();
    pull.bind("tcp://127.0.0.1:*").unwrap();
    let ep = pull.get_last_endpoint().unwrap().unwrap();
    let receiver = std::thread::spawn(move || {
        for i in 0..10usize {
            let m = pull.recv_multipart(0).unwrap();
            assert_eq!(m.len(), 2);
            assert_eq!(m[1].len(), 100_000 * (i + 1));
        }
    });
    rt().block_on(async {
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        for i in 0..10usize {
            push.send(Msg::from([
                Bytes::from_static(b"hdr"),
                Bytes::from(vec![1u8; 100_000 * (i + 1)]),
            ]))
            .await
            .unwrap();
        }
        push.flush().await.unwrap();
    });
    receiver.join().unwrap();
}
