//! `zmq_poll` over mixed socket kinds.
//!
//! Every test runs on whichever runtime the crate is built with, so the suite
//! covers compio, tokio and smol across the three feature sets.

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::poll::{PollEvents, PollItem, poll};
use monocoque::zmq::{
    DealerSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket, RouterSocket,
    SocketOptions, StreamSocket, SubSocket,
};
use std::future::Future;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const IN: PollEvents = PollEvents::POLLIN;
const OUT: PollEvents = PollEvents::POLLOUT;

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

/// Wait until `sock` has `n` peers.
macro_rules! wait_peers {
    ($sock:expr, $n:expr) => {
        let deadline = Instant::now() + Duration::from_secs(5);
        while $sock.peer_count() < $n {
            assert!(Instant::now() < deadline, "peer never connected");
            monocoque::rt::sleep(Duration::from_millis(1)).await;
        }
    };
}

#[test]
fn reports_only_the_ready_items_and_consumes_nothing() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        let ep_pull = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep_pull).await.unwrap();

        let mut publ = PubSocket::new();
        let ep_pub = publ.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(opts());
        sub.connect(&ep_pub).await.unwrap();
        sub.subscribe("t").await.unwrap();

        let mut router = RouterSocket::with_options(opts());
        let ep_router = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer = DealerSocket::new();
        dealer.connect(&ep_router).await.unwrap();

        wait_peers!(pull, 1);
        wait_peers!(router, 1);
        wait_peers!(publ, 1);
        // Let the subscription reach the publisher.
        while publ.subscriber_prefixes(publ.peer_ids()[0]) == 0 {
            monocoque::rt::sleep(Duration::from_millis(1)).await;
        }

        {
            let mut items = [
                PollItem::new(&pull, IN),
                PollItem::new(&sub, IN),
                PollItem::new(&router, IN),
            ];
            assert_eq!(poll(&mut items, Some(Duration::ZERO)).await.unwrap(), 0);
        }

        // A message SUB filters out must not make it readable.
        publ.send("other").await.unwrap();
        publ.send("topic").await.unwrap();

        let mut items = [
            PollItem::new(&pull, IN),
            PollItem::new(&sub, IN),
            PollItem::new(&router, IN),
        ];
        assert_eq!(
            poll(&mut items, Some(Duration::from_secs(5)))
                .await
                .unwrap(),
            1
        );
        assert!(!items[0].is_readable());
        assert!(items[1].is_readable());
        assert_eq!(items[1].revents(), IN);
        assert!(!items[2].is_readable());

        // Polling again finds the same message: nothing was consumed.
        assert_eq!(poll(&mut items, Some(Duration::ZERO)).await.unwrap(), 1);
        assert!(items[1].is_readable());
        assert_eq!(sub.recv().await.unwrap(), [b"topic"]);

        push.send("job").await.unwrap();
        dealer.send("hi").await.unwrap();
        let mut items = [
            PollItem::new(&pull, IN),
            PollItem::new(&sub, IN),
            PollItem::new(&router, IN),
        ];
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            poll(&mut items, Some(Duration::from_secs(5)))
                .await
                .unwrap();
            if items[0].is_readable() && items[2].is_readable() {
                break;
            }
            assert!(Instant::now() < deadline);
            // Let the other message land before looking again.
            monocoque::rt::sleep(Duration::from_millis(1)).await;
        }
        assert!(!items[1].is_readable());
        // ROUTER never waits to send, so it is always writable.
        let mut items = [PollItem::new(&router, IN | OUT)];
        assert_eq!(poll(&mut items, Some(Duration::ZERO)).await.unwrap(), 1);
        assert_eq!(items[0].revents(), IN | OUT);
        assert_eq!(pull.recv().await.unwrap(), [b"job"]);
        assert_eq!(router.recv().await.unwrap().len(), 2);
    });
}

#[test]
fn timeout_zero_and_expiry() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let sub = SubSocket::new();

        let mut items = [PollItem::new(&pull, IN), PollItem::new(&sub, IN)];
        let t = Instant::now();
        assert_eq!(poll(&mut items, Some(Duration::ZERO)).await.unwrap(), 0);
        assert!(t.elapsed() < Duration::from_millis(100));

        let t = Instant::now();
        assert_eq!(
            poll(&mut items, Some(Duration::from_millis(80)))
                .await
                .unwrap(),
            0
        );
        assert!(t.elapsed() >= Duration::from_millis(80));
        assert!(items.iter().all(|i| i.revents().is_empty()));

        // An expired poll handed the socket back to its driver: it still
        // accepts and receives.
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        push.send("after").await.unwrap();
        assert_eq!(pull.recv().await.unwrap(), [b"after"]);
    });
}

#[test]
fn pollout_on_a_full_socket_becomes_ready_after_drain() {
    let small = SocketOptions::default()
        .with_send_hwm(2)
        .with_recv_hwm(2)
        .with_sndbuf(16 * 1024)
        .with_rcvbuf(16 * 1024)
        .with_send_timeout(Duration::from_millis(20));
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<usize>();
    let receiver = thread::spawn({
        let small = small.clone();
        move || {
            LocalRuntime::new().unwrap().block_on(async move {
                let mut pull =
                    PullSocket::with_options(small.with_recv_timeout(Duration::from_secs(1)));
                pull.connect(&ep_rx.recv().unwrap()).await.unwrap();
                // Wait without blocking the thread, so this runtime can still
                // complete the handshake.
                while go_rx.try_recv().is_err() {
                    monocoque::rt::sleep(Duration::from_millis(5)).await;
                }
                let mut n = 0;
                while pull.recv().await.is_ok() {
                    n += 1;
                    if done_rx.try_recv().is_ok() {
                        break;
                    }
                }
                n
            })
        }
    });

    LocalRuntime::new().unwrap().block_on(async move {
        let mut push = PushSocket::with_options(small);
        ep_tx
            .send(push.bind("tcp://127.0.0.1:0").await.unwrap())
            .unwrap();
        wait_peers!(push, 1);
        let payload = Bytes::from(vec![7u8; 64 * 1024]);
        // Fill until the socket stays full across a pause: the kernel may
        // still take queued bytes for a moment after it first looks full.
        let mut sent = 0;
        loop {
            while push.can_send() {
                push.send(payload.clone()).await.unwrap();
                sent += 1;
                assert!(sent < 10_000, "the socket never filled");
            }
            monocoque::rt::sleep(Duration::from_millis(100)).await;
            if !push.can_send() {
                break;
            }
        }

        let mut items = [PollItem::new(&push, OUT)];
        assert_eq!(poll(&mut items, Some(Duration::ZERO)).await.unwrap(), 0);
        assert!(
            poll(&mut items, Some(Duration::from_millis(50)))
                .await
                .unwrap()
                == 0,
            "a full socket stays unwritable while nobody reads"
        );

        go_tx.send(()).unwrap();
        assert_eq!(
            poll(&mut items, Some(Duration::from_secs(10)))
                .await
                .unwrap(),
            1
        );
        assert_eq!(items[0].revents(), OUT);
        push.send("last").await.unwrap();
        done_tx.send(0).unwrap();
        // Only nudges the receiver to notice `done`; the queue may be full
        // again, so a timeout here is fine.
        let _ = push.send("wake").await;
    });
    assert!(receiver.join().unwrap() > 0);
}

#[test]
fn woken_by_a_peer_on_another_thread() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let idle = DealerSocket::new();

        let sender = thread::spawn(move || {
            LocalRuntime::new().unwrap().block_on(async move {
                monocoque::rt::sleep(Duration::from_millis(150)).await;
                let mut push = PushSocket::new();
                push.connect(&ep).await.unwrap();
                push.send("from afar").await.unwrap();
                push.close().await.unwrap();
            });
        });

        let mut items = [PollItem::new(&idle, IN), PollItem::new(&pull, IN)];
        let t = Instant::now();
        assert_eq!(poll(&mut items, None).await.unwrap(), 1);
        assert!(items[1].is_readable());
        assert!(t.elapsed() >= Duration::from_millis(100));
        assert_eq!(pull.recv().await.unwrap(), [b"from afar"]);
        sender.join().unwrap();
    });
}

#[test]
fn inproc_peer_on_another_thread_wakes_the_poll() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        pull.bind("inproc://poll-cross-thread").await.unwrap();
        let sender = thread::spawn(|| {
            LocalRuntime::new().unwrap().block_on(async {
                let mut push = PushSocket::new();
                push.connect("inproc://poll-cross-thread").await.unwrap();
                monocoque::rt::sleep(Duration::from_millis(100)).await;
                push.send("inproc").await.unwrap();
                monocoque::rt::sleep(Duration::from_millis(200)).await;
            });
        });
        let mut items = [PollItem::new(&pull, IN)];
        assert_eq!(
            poll(&mut items, Some(Duration::from_secs(5)))
                .await
                .unwrap(),
            1
        );
        assert_eq!(pull.recv().await.unwrap(), [b"inproc"]);
        sender.join().unwrap();
    });
}

#[test]
fn req_rep_follow_their_state_machines() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut rep = RepSocket::with_options(opts());
        let ep = rep.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut req = ReqSocket::with_options(opts());
        req.connect(&ep).await.unwrap();
        wait_peers!(rep, 1);
        wait_peers!(req, 1);

        let both = IN | OUT;
        {
            let mut items = [PollItem::new(&req, both), PollItem::new(&rep, both)];
            assert_eq!(poll(&mut items, Some(Duration::ZERO)).await.unwrap(), 1);
            assert_eq!(items[0].revents(), OUT, "REQ may only send first");
            assert!(items[1].revents().is_empty(), "REP has nothing to do");
        }

        req.send("q").await.unwrap();
        {
            let mut items = [PollItem::new(&req, both), PollItem::new(&rep, both)];
            assert_eq!(
                poll(&mut items, Some(Duration::from_secs(5)))
                    .await
                    .unwrap(),
                1
            );
            assert!(items[0].revents().is_empty(), "REQ waits for its reply");
            assert_eq!(items[1].revents(), IN);
        }
        assert_eq!(rep.recv().await.unwrap(), [b"q"]);
        {
            let mut items = [PollItem::new(&rep, both)];
            assert_eq!(poll(&mut items, Some(Duration::ZERO)).await.unwrap(), 1);
            assert_eq!(items[0].revents(), OUT, "REP owes a reply");
        }
        rep.send("a").await.unwrap();
        let mut items = [PollItem::new(&req, IN)];
        assert_eq!(
            poll(&mut items, Some(Duration::from_secs(5)))
                .await
                .unwrap(),
            1
        );
        assert_eq!(req.recv().await.unwrap(), [b"a"]);
    });
}

#[test]
fn stream_sockets_mix_with_zmtp_sockets() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut stream = StreamSocket::with_options(opts());
        let ep = stream.bind("tcp://127.0.0.1:0").await.unwrap();
        let pull = PullSocket::new();
        let addr = ep.trim_start_matches("tcp://").to_string();
        let client = thread::spawn(move || {
            use std::io::Write;
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            s.write_all(b"raw").unwrap();
            thread::sleep(Duration::from_millis(300));
        });

        // The connect notification, then the data.
        for _ in 0..2 {
            let mut items = [PollItem::new(&pull, IN), PollItem::new(&stream, IN)];
            assert_eq!(
                poll(&mut items, Some(Duration::from_secs(5)))
                    .await
                    .unwrap(),
                1
            );
            assert!(items[1].is_readable());
            assert_eq!(stream.recv().await.unwrap().len(), 2);
        }
        client.join().unwrap();
    });
}

#[test]
fn a_dropped_poll_leaves_the_socket_working() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        {
            let mut items = [PollItem::new(&pull, IN)];
            let wait = poll(&mut items, None);
            let mut wait = std::pin::pin!(wait);
            // Poll it once so it parks, then drop it.
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            assert!(wait.as_mut().poll(&mut cx).is_pending());
        }
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        push.send("still here").await.unwrap();
        assert_eq!(pull.recv().await.unwrap(), [b"still here"]);
    });
}
