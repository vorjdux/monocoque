//! `proxy` and `proxy_steerable` over the multiplexer sockets, including
//! real libzmq peers on both sides of a broker.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::proxy::{NoCapture, ProxyCommand, proxy, proxy_steerable};
use monocoque::zmq::{
    DealerSocket, MonocoqueError, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket,
    ReqSocket, RouterSocket, SocketOptions, SubSocket, XPubSocket, XSubSocket,
};
use std::time::Duration;

fn quick(ms: u64) -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_millis(ms))
}

/// ROUTER/DEALER broker between REQ clients and REP workers.
#[test]
fn router_dealer_broker() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut frontend = RouterSocket::new();
        let front_ep = frontend.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut backend = DealerSocket::new();
        let back_ep = backend.bind("tcp://127.0.0.1:0").await.unwrap();
        monocoque::rt::spawn_detached(async move {
            let _ = proxy(&mut frontend, &mut backend, None::<&mut NoCapture>).await;
        });

        for w in 0..2 {
            let back_ep = back_ep.clone();
            monocoque::rt::spawn_detached(async move {
                let mut rep = RepSocket::new();
                rep.connect(&back_ep).await.unwrap();
                loop {
                    let req = rep.recv_owned().await.unwrap();
                    let mut reply = format!("w{w}:").into_bytes();
                    reply.extend_from_slice(req.frames().next().unwrap());
                    rep.send(reply).await.unwrap();
                }
            });
        }

        let mut req = ReqSocket::new();
        req.connect(&front_ep).await.unwrap();
        let mut seen = [0usize; 2];
        for i in 0..20 {
            req.send(format!("r{i}")).await.unwrap();
            let reply = req.recv_owned().await.unwrap();
            let text = String::from_utf8(reply.frames().next().unwrap().to_vec()).unwrap();
            assert!(text.ends_with(&format!(":r{i}")), "{text}");
            seen[usize::from(text.as_bytes()[1] - b'0')] += 1;
        }
        assert_eq!(seen.iter().sum::<usize>(), 20);
    });
}

/// XSUB/XPUB forwarder: subscriptions travel upstream, messages downstream,
/// and the capture socket sees every forwarded message.
#[test]
fn xsub_xpub_forwarder_with_capture() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut xsub = XSubSocket::new();
        let up_ep = xsub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut xpub = XPubSocket::new();
        let down_ep = xpub.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut capture = PushSocket::new();
        let cap_ep = capture.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut cap_pull = PullSocket::with_options(quick(2000));
        cap_pull.connect(&cap_ep).await.unwrap();
        monocoque::rt::spawn_detached(async move {
            let _ = proxy(&mut xsub, &mut xpub, Some(&mut capture)).await;
        });

        let mut publisher = PubSocket::new();
        publisher.connect(&up_ep).await.unwrap();
        let mut sub = SubSocket::with_options(quick(50));
        sub.subscribe(b"news").await.unwrap();
        sub.connect(&down_ep).await.unwrap();

        // Publish until the subscription has made it through the proxy.
        let mut got = None;
        for _ in 0..200 {
            publisher.send("news hello").await.unwrap();
            if let Ok(m) = sub.recv_owned().await {
                got = Some(m);
                break;
            }
        }
        assert_eq!(got.expect("no message through the forwarder"), [b"news hello"]);
        // The capture saw the subscription going up as well as the data.
        let first = cap_pull.recv_owned().await.unwrap();
        assert_eq!(first, [b"\x01news"]);
    });
}

/// PAUSE holds messages in the sockets, RESUME delivers them, STATISTICS
/// reports libzmq's eight counters, TERMINATE returns.
#[test]
fn steerable_pause_resume_statistics_terminate() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut frontend = PullSocket::new();
        let front_ep = frontend.bind("inproc://steer-front").await.unwrap();
        let mut backend = PushSocket::new();
        let back_ep = backend.bind("inproc://steer-back").await.unwrap();
        let mut control = PairSocket::new();
        let ctl_ep = control.bind("inproc://steer-ctl").await.unwrap();
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let done2 = done.clone();
        monocoque::rt::spawn_detached(async move {
            proxy_steerable(&mut frontend, &mut backend, None::<&mut NoCapture>, &mut control)
                .await
                .unwrap();
            done2.set(true);
        });

        let mut push = PushSocket::new();
        push.connect(&front_ep).await.unwrap();
        let mut pull = PullSocket::with_options(quick(100));
        pull.connect(&back_ep).await.unwrap();
        let mut ctl = PairSocket::with_options(quick(2000));
        ctl.connect(&ctl_ep).await.unwrap();

        push.send("one").await.unwrap();
        assert_eq!(pull.recv_owned().await.unwrap(), [b"one"]);

        ctl.send(ProxyCommand::Pause.as_bytes()).await.unwrap();
        monocoque::rt::sleep(Duration::from_millis(20)).await;
        push.send("two").await.unwrap();
        assert!(matches!(pull.recv().await, Err(MonocoqueError::TimedOut)));

        ctl.send(ProxyCommand::Resume.as_bytes()).await.unwrap();
        assert_eq!(pull.recv_owned().await.unwrap(), [b"two"]);

        ctl.send(ProxyCommand::Statistics.as_bytes()).await.unwrap();
        let stats = ctl.recv_owned().await.unwrap();
        let n: Vec<u64> = stats
            .frames()
            .map(|f| u64::from_le_bytes(f[..].try_into().unwrap()))
            .collect();
        // frontend: 2 in, 6 bytes in, 0 out; backend: 0 in, 2 out, 6 bytes out
        assert_eq!(n, [2, 6, 0, 0, 0, 0, 2, 6]);

        ctl.send(ProxyCommand::Terminate.as_bytes()).await.unwrap();
        for _ in 0..1000 {
            if done.get() {
                return;
            }
            monocoque::rt::sleep(Duration::from_millis(1)).await;
        }
        panic!("proxy did not terminate");
    });
}

/// A libzmq REQ client and a libzmq REP worker talk through our broker.
#[test]
fn libzmq_peers_through_the_broker() {
    let (ep_tx, ep_rx) = std::sync::mpsc::channel::<(String, String)>();
    let broker = std::thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut frontend = RouterSocket::new();
            let front_ep = frontend.bind("tcp://127.0.0.1:0").await.unwrap();
            let mut backend = DealerSocket::new();
            let back_ep = backend.bind("tcp://127.0.0.1:0").await.unwrap();
            ep_tx.send((front_ep, back_ep)).unwrap();
            let mut control = PairSocket::new();
            let _ = control.bind("inproc://libzmq-broker-ctl").await.unwrap();
            let _ = monocoque::rt::timeout(
                Duration::from_secs(10),
                proxy_steerable(&mut frontend, &mut backend, None::<&mut NoCapture>, &mut control),
            )
            .await;
        });
    });
    let (front_ep, back_ep) = ep_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let worker = std::thread::spawn({
        let ctx = ctx.clone();
        move || {
            let rep = ctx.socket(zmq::REP).unwrap();
            rep.connect(&back_ep).unwrap();
            for _ in 0..5 {
                let m = rep.recv_bytes(0).unwrap();
                rep.send([b"ok:".as_slice(), &m].concat(), 0).unwrap();
            }
        }
    });
    let req = ctx.socket(zmq::REQ).unwrap();
    req.set_rcvtimeo(5000).unwrap();
    req.connect(&front_ep).unwrap();
    for i in 0..5 {
        req.send(format!("q{i}").as_bytes(), 0).unwrap();
        assert_eq!(req.recv_bytes(0).unwrap(), format!("ok:q{i}").into_bytes());
    }
    worker.join().unwrap();
    drop(req);
    drop(ctx);
    let _ = broker;
}
