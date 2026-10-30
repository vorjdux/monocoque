//! `SocketCore` exercised over a toy length-prefixed protocol.

use super::*;
use crate::rt::LocalRuntime;
use bytes::Buf;

/// One frame per message: 4-byte big-endian length, then the bytes.
struct Lp;

struct LpCodec;

impl Codec for LpCodec {
    fn decode(
        &mut self,
        buf: &mut BytesMut,
        deliver: &mut dyn FnMut(Msg),
        _replies: &mut OutQueue,
    ) -> Result<()> {
        loop {
            if buf.len() < 4 {
                return Ok(());
            }
            let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
            if buf.len() < 4 + len {
                buf.reserve(4 + len - buf.len());
                return Ok(());
            }
            buf.advance(4);
            if len <= crate::msg::INLINE_CAP {
                deliver(Msg::copy_from_slice(&buf[..len]));
                buf.advance(len);
            } else {
                deliver(Msg::from(buf.split_to(len).freeze()));
            }
        }
    }

    fn encode(&mut self, msg: &Msg, out: &mut OutQueue) -> Result<()> {
        let body = msg.first().unwrap_or_default();
        let b = out.buf();
        b.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
        b.extend_from_slice(body);
        Ok(())
    }

    fn shares_encoding(&self) -> bool {
        true
    }
}

impl Protocol for Lp {
    type Config = ();
    type Codec = LpCodec;

    async fn handshake(
        _cfg: Rc<()>,
        _options: Rc<SocketOptions>,
        stream: Rc<Stream>,
        _outgoing: bool,
    ) -> Result<(LpCodec, PeerInfo)> {
        let info = PeerInfo {
            socket_type: "LP",
            address: crate::rt::PeerAddress::zap_address(&*stream),
            ..PeerInfo::default()
        };
        Ok((LpCodec, info))
    }

    fn socket_type(_cfg: &()) -> &'static str {
        "LP"
    }

    fn compatible(_cfg: &(), peer: &str) -> bool {
        peer == "LP"
    }

    fn encode_shared(_cfg: &(), msg: &Msg, buf: &mut BytesMut) {
        let body = msg.first().unwrap_or_default();
        buf.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
        buf.extend_from_slice(body);
    }
}

type Core = Shared<Lp, ()>;

fn core(options: SocketOptions) -> Core {
    SocketCore::new((), options, ())
}

async fn recv(c: &Core) -> (PeerId, Msg) {
    op(c, SocketCore::poll_recv).await
}

async fn send(c: &Core, msg: Msg) -> PeerId {
    let mut slot = Some(msg);
    op(c, move |core, cx| core.poll_send_lb(cx, &mut slot))
        .await
        .unwrap()
}

async fn wait_peers(c: &Core, n: usize) {
    op(c, |core, cx| {
        core.progress();
        if core.peer_count() >= n {
            Poll::Ready(())
        } else {
            core.park(cx);
            Poll::Pending
        }
    })
    .await;
}

#[test]
fn ping_pong_over_tcp() {
    LocalRuntime::new().unwrap().block_on(async {
        let server = core(SocketOptions::default());
        let ep = SocketCore::bind(&server, "tcp://127.0.0.1:0")
            .await
            .unwrap();
        let client = core(SocketOptions::default());
        SocketCore::connect(&client, &ep.to_string()).unwrap();

        for i in 0u32..200 {
            let body = vec![u8::try_from(i % 251).unwrap(); (i as usize * 7) % 300];
            send(&client, Msg::from(body.clone())).await;
            let (from, msg) = recv(&server).await;
            assert_eq!(msg, [&body[..]]);
            send(&server, msg).await;
            let (_, echo) = recv(&client).await;
            assert_eq!(echo, [&body[..]]);
            assert!(server.borrow().is_live(from));
        }
    });
}

#[test]
fn fair_queue_round_robins_between_peers() {
    LocalRuntime::new().unwrap().block_on(async {
        let server = core(SocketOptions::default());
        let ep = SocketCore::bind(&server, "tcp://127.0.0.1:0")
            .await
            .unwrap();
        let a = core(SocketOptions::default());
        let b = core(SocketOptions::default());
        SocketCore::connect(&a, &ep.to_string()).unwrap();
        SocketCore::connect(&b, &ep.to_string()).unwrap();
        wait_peers(&server, 2).await;
        for _ in 0..4 {
            send(&a, Msg::from("a")).await;
            send(&b, Msg::from("b")).await;
        }
        // Let all eight arrive before receiving any.
        let mut got = Vec::new();
        while got.len() < 8 {
            let (_, m) = recv(&server).await;
            got.push(m.first().unwrap()[0]);
            crate::rt::sleep(Duration::from_millis(1)).await;
        }
        // Once both peers have messages queued, consecutive receives alternate.
        let alternations = got.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(alternations >= 3, "fair queue did not alternate: {got:?}");
        got.sort_unstable();
        assert_eq!(got, b"aaaabbbb");
    });
}

#[test]
fn recv_hwm_backpressures_only_the_slow_peer() {
    LocalRuntime::new().unwrap().block_on(async {
        let opts = SocketOptions::default().with_recv_hwm(4).with_send_hwm(4);
        let server = core(opts.clone());
        let ep = SocketCore::bind(&server, "tcp://127.0.0.1:0")
            .await
            .unwrap();
        let client = core(opts);
        SocketCore::connect(&client, &ep.to_string()).unwrap();
        wait_peers(&server, 1).await;
        // The server never receives. Its ready queue stops at 4, then the
        // kernel buffers fill, then the client's queue stops at 4: sends must
        // run out of room rather than queue without bound.
        let mut sent = 0usize;
        loop {
            if !client.borrow_mut().any_writable() {
                crate::rt::sleep(Duration::from_millis(20)).await;
                if !client.borrow_mut().any_writable() {
                    break;
                }
                continue;
            }
            send(&client, Msg::from(vec![0u8; 16 * 1024])).await;
            sent += 1;
            assert!(sent < 10_000, "send never hit the HWM");
            crate::rt::sleep(Duration::from_millis(1)).await;
        }
        assert!(sent >= 8, "blocked before the HWMs filled: {sent}");
        // Draining the server releases the client.
        for _ in 0..sent {
            recv(&server).await;
        }
        assert!(client.borrow_mut().any_writable());
    });
}

#[test]
fn reconnects_after_the_peer_goes_away() {
    LocalRuntime::new().unwrap().block_on(async {
        let opts = SocketOptions::default().with_reconnect_ivl(Duration::from_millis(10));
        let server = core(opts.clone());
        let ep = SocketCore::bind(&server, "tcp://127.0.0.1:0")
            .await
            .unwrap();
        let client = core(opts.clone());
        SocketCore::connect(&client, &ep.to_string()).unwrap();
        send(&client, Msg::from("one")).await;
        assert_eq!(recv(&server).await.1, [b"one"]);

        // Kill the server; bring a new one up on the same port.
        server.borrow_mut().close();
        drop(server);
        crate::rt::sleep(Duration::from_millis(50)).await;
        let server = core(opts);
        SocketCore::bind(&server, &ep.to_string()).await.unwrap();
        wait_peers(&client, 1).await;
        send(&client, Msg::from("two")).await;
        assert_eq!(recv(&server).await.1, [b"two"]);
    });
}

#[test]
fn stops_dialing_after_max_reconnect_attempts() {
    LocalRuntime::new().unwrap().block_on(async {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let ep = format!("tcp://127.0.0.1:{port}");
        let opts = SocketOptions::default().with_reconnect_ivl(Duration::from_millis(10));
        let limited = core(opts.clone().with_max_reconnect_attempts(Some(2)));
        SocketCore::connect(&limited, &ep).unwrap();
        let unlimited = core(opts.clone());
        SocketCore::connect(&unlimited, &ep).unwrap();
        // Both fail well past two attempts before anything listens.
        crate::rt::sleep(Duration::from_millis(200)).await;
        let server = core(opts);
        SocketCore::bind(&server, &ep).await.unwrap();
        wait_peers(&unlimited, 1).await;
        crate::rt::sleep(Duration::from_millis(100)).await;
        assert_eq!(limited.borrow().peer_count(), 0);
    });
}

#[test]
fn inproc_same_thread_moves_messages_without_a_codec() {
    LocalRuntime::new().unwrap().block_on(async {
        let a = core(SocketOptions::default());
        SocketCore::bind(&a, "inproc://mux-same-thread")
            .await
            .unwrap();
        let b = core(SocketOptions::default());
        SocketCore::connect(&b, "inproc://mux-same-thread").unwrap();
        let big = Msg::from(vec![9u8; 100_000]);
        send(&b, big.clone()).await;
        let (_, got) = recv(&a).await;
        assert_eq!(got, big);
        send(&a, Msg::from("back")).await;
        assert_eq!(recv(&b).await.1, [b"back"]);
    });
}

#[test]
fn inproc_crosses_threads() {
    let (tx, rx) = std::sync::mpsc::channel();
    let binder = std::thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let a = core(SocketOptions::default());
            SocketCore::bind(&a, "inproc://mux-cross-thread")
                .await
                .unwrap();
            tx.send(()).unwrap();
            let (_, m) = recv(&a).await;
            send(&a, m).await;
            // Give the reply time to be read before the socket goes.
            crate::rt::sleep(Duration::from_millis(50)).await;
        });
    });
    rx.recv().unwrap();
    LocalRuntime::new().unwrap().block_on(async {
        let b = core(SocketOptions::default());
        SocketCore::connect(&b, "inproc://mux-cross-thread").unwrap();
        send(&b, Msg::from("ping")).await;
        assert_eq!(recv(&b).await.1, [b"ping"]);
    });
    binder.join().unwrap();
}

#[test]
fn close_flushes_queued_messages_within_linger() {
    LocalRuntime::new().unwrap().block_on(async {
        let server = core(SocketOptions::default());
        let ep = SocketCore::bind(&server, "tcp://127.0.0.1:0")
            .await
            .unwrap();
        let client = core(SocketOptions::default().with_linger(Some(Duration::from_secs(5))));
        SocketCore::connect(&client, &ep.to_string()).unwrap();
        wait_peers(&client, 1).await;
        for i in 0..100u8 {
            send(&client, Msg::from(vec![i; 1000])).await;
        }
        client.borrow_mut().close();
        drop(client);
        for i in 0..100u8 {
            let (_, m) = recv(&server).await;
            assert_eq!(m.first().unwrap()[0], i);
        }
    });
}

#[test]
fn fanout_shares_one_encoding() {
    LocalRuntime::new().unwrap().block_on(async {
        let hub = core(SocketOptions::default());
        let ep = SocketCore::bind(&hub, "tcp://127.0.0.1:0").await.unwrap();
        let subs: Vec<Core> = (0..3).map(|_| core(SocketOptions::default())).collect();
        for s in &subs {
            SocketCore::connect(s, &ep.to_string()).unwrap();
        }
        wait_peers(&hub, 3).await;
        let n = hub.borrow_mut().fanout(&Msg::from("all"), |_, _| true);
        assert_eq!(n, 3);
        for s in &subs {
            assert_eq!(recv(s).await.1, [b"all"]);
        }
    });
}
