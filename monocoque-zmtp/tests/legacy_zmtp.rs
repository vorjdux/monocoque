//! ZMTP 1.0 and 2.0 peers. No libzmq old enough to speak them is packaged
//! any more, so each peer here is scripted byte for byte from the specs
//! (RFC 13 for 1.0, RFC 15 for 2.0), written the way libzmq 2.x and 3.x put
//! them on the wire.

// The scripted peers only ever build frames far below every length limit.
#![allow(clippy::cast_possible_truncation)]

use bytes::Bytes;
use monocoque_core::msg::Msg;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{LocalRuntime, sleep, timeout};
use monocoque_zmtp::sock::kinds::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

fn rt() -> LocalRuntime {
    LocalRuntime::new().unwrap()
}

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

fn connect(endpoint: &str) -> TcpStream {
    let addr = endpoint.strip_prefix("tcp://").unwrap();
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

fn read_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).unwrap();
    buf
}

/// True once the peer has closed the connection, false if it is still open
/// when the read timeout runs out. Closing with our bytes unread resets it.
fn closed(s: &mut TcpStream) -> bool {
    let mut buf = [0u8; 64];
    loop {
        match s.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(e) => return e.kind() == std::io::ErrorKind::ConnectionReset,
        }
    }
}

/// Open a ZMTP 2.0 connection as socket type `ty` with `identity`, checking
/// the greeting we get back.
fn v2_handshake(endpoint: &str, ty: u8, identity: &[u8], want_ty: u8) -> TcpStream {
    let mut s = connect(endpoint);
    let mut greeting = vec![0xFF, 0, 0, 0, 0, 0, 0, 0, 1, 0x7F, 1, ty];
    greeting.push(0);
    greeting.push(identity.len() as u8);
    greeting.extend_from_slice(identity);
    s.write_all(&greeting).unwrap();
    let theirs = read_n(&mut s, 12);
    assert_eq!(theirs[0], 0xFF);
    assert_eq!(theirs[9] & 1, 1);
    assert_eq!(theirs[10], 3, "a ZMTP 3 socket announces major version 3");
    assert_eq!(theirs[11], want_ty, "ZMTP 2.0 socket type number");
    let id = read_n(&mut s, 2);
    assert_eq!(id[0], 0, "identity frame flags");
    read_n(&mut s, usize::from(id[1]));
    s
}

/// Open a ZMTP 1.0 connection: the identity frame is all there is.
fn v1_handshake(endpoint: &str, identity: &[u8]) -> TcpStream {
    let mut s = connect(endpoint);
    let mut hello = vec![identity.len() as u8 + 1, 0];
    hello.extend_from_slice(identity);
    s.write_all(&hello).unwrap();
    // Our signature reads to a 1.0 peer as a long identity frame header.
    let sig = read_n(&mut s, 10);
    assert_eq!(sig[0], 0xFF);
    let len = u64::from_be_bytes(sig[1..9].try_into().unwrap());
    assert_eq!(len, 1, "anonymous identity: just the flags byte");
    s
}

fn v2_frame(more: bool, body: &[u8]) -> Vec<u8> {
    let flags = u8::from(more);
    let mut f = if body.len() < 256 {
        vec![flags, body.len() as u8]
    } else {
        let mut f = vec![flags | 2];
        f.extend_from_slice(&(body.len() as u64).to_be_bytes());
        f
    };
    f.extend_from_slice(body);
    f
}

fn read_v2_frame(s: &mut TcpStream) -> (bool, Vec<u8>) {
    let head = read_n(s, 2);
    let len = if head[0] & 2 != 0 {
        let mut long = vec![head[1]];
        long.extend(read_n(s, 7));
        u64::from_be_bytes(long.try_into().unwrap()) as usize
    } else {
        usize::from(head[1])
    };
    (head[0] & 1 != 0, read_n(s, len))
}

fn v1_frame(more: bool, body: &[u8]) -> Vec<u8> {
    let mut f = if body.len() + 1 < 0xFF {
        vec![body.len() as u8 + 1]
    } else {
        let mut f = vec![0xFF];
        f.extend_from_slice(&(body.len() as u64 + 1).to_be_bytes());
        f
    };
    f.push(u8::from(more));
    f.extend_from_slice(body);
    f
}

fn read_v1_frame(s: &mut TcpStream) -> (bool, Vec<u8>) {
    let first = read_n(s, 1)[0];
    let len = if first == 0xFF {
        u64::from_be_bytes(read_n(s, 8).try_into().unwrap()) as usize
    } else {
        usize::from(first)
    };
    let flags = read_n(s, 1)[0];
    (flags & 1 != 0, read_n(s, len - 1))
}

#[test]
fn router_talks_to_a_zmtp_2_dealer() {
    rt().block_on(async {
        let mut router = RouterSocket::with_options(opts());
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let peer = thread::spawn(move || {
            let mut s = v2_handshake(&ep, 5, b"abc", 6);
            s.write_all(&v2_frame(false, b"hello")).unwrap();
            let big = vec![7u8; 300];
            s.write_all(&v2_frame(true, b"")).unwrap();
            s.write_all(&v2_frame(false, &big)).unwrap();
            let a = read_v2_frame(&mut s);
            assert_eq!(a, (false, b"world".to_vec()));
            let b = read_v2_frame(&mut s);
            assert_eq!(b, (false, vec![9u8; 400]));
        });

        let m = router.recv_owned().await.unwrap();
        let frames: Vec<&[u8]> = m.frames().collect();
        assert_eq!(frames, [&b"abc"[..], b"hello"]);
        let m = router.recv_owned().await.unwrap();
        let frames: Vec<&[u8]> = m.frames().collect();
        assert_eq!(frames, [&b"abc"[..], b"", &[7u8; 300][..]]);

        router
            .send(Msg::from([
                Bytes::from_static(b"abc"),
                Bytes::from_static(b"world"),
            ]))
            .await
            .unwrap();
        router
            .send(Msg::from([
                Bytes::from_static(b"abc"),
                Bytes::from(vec![9u8; 400]),
            ]))
            .await
            .unwrap();
        thread_result(peer).await;
    });
}

#[test]
fn pull_receives_from_a_zmtp_1_push() {
    rt().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let peer = thread::spawn(move || {
            let mut s = v1_handshake(&ep, b"");
            s.write_all(&v1_frame(true, b"part")).unwrap();
            s.write_all(&v1_frame(false, &[3u8; 600])).unwrap();
            s.write_all(&v1_frame(false, b"tail")).unwrap();
            s
        });
        let m = pull.recv_owned().await.unwrap();
        let frames: Vec<&[u8]> = m.frames().collect();
        assert_eq!(frames, [&b"part"[..], &[3u8; 600][..]]);
        assert_eq!(pull.recv_owned().await.unwrap().first(), Some(&b"tail"[..]));
        drop(thread_result(peer).await);
    });
}

#[test]
fn push_sends_to_a_zmtp_1_pull() {
    rt().block_on(async {
        let mut push = PushSocket::with_options(opts());
        let ep = push.bind("tcp://127.0.0.1:0").await.unwrap();
        let peer = thread::spawn(move || {
            let mut s = v1_handshake(&ep, b"");
            assert_eq!(read_v1_frame(&mut s), (true, b"a".to_vec()));
            assert_eq!(read_v1_frame(&mut s), (false, vec![5u8; 300]));
        });
        push.send(Msg::from([
            Bytes::from_static(b"a"),
            Bytes::from(vec![5u8; 300]),
        ]))
        .await
        .unwrap();
        thread_result(peer).await;
    });
}

#[test]
fn pub_sends_everything_to_a_zmtp_1_subscriber() {
    rt().block_on(async {
        let mut publisher = PubSocket::with_options(opts());
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let peer = thread::spawn(move || {
            let mut s = v1_handshake(&ep, b"");
            // A 1.0 subscriber filters for itself and never subscribes.
            assert_eq!(read_v1_frame(&mut s), (false, b"news".to_vec()));
        });
        while publisher.send(Msg::from("news")).await.unwrap() == 0 {
            sleep(Duration::from_millis(5)).await;
        }
        thread_result(peer).await;
    });
}

#[test]
fn zmtp_2_subscriber_subscriptions_filter() {
    rt().block_on(async {
        let mut publisher = PubSocket::with_options(opts());
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let peer = thread::spawn(move || {
            let mut s = v2_handshake(&ep, 2, b"", 1);
            s.write_all(&v2_frame(false, b"\x01a")).unwrap();
            assert_eq!(read_v2_frame(&mut s), (false, b"a1".to_vec()));
        });
        // "b0" matches nothing, so the first frame the peer sees is "a1".
        loop {
            publisher.send(Msg::from("b0")).await.unwrap();
            if publisher.send(Msg::from("a1")).await.unwrap() > 0 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
        thread_result(peer).await;
    });
}

#[test]
fn zmtp_2_peer_of_the_wrong_type_is_refused() {
    rt().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let peer = thread::spawn(move || {
            let mut s = connect(&ep);
            // A SUB cannot talk to a PULL.
            s.write_all(&[0xFF, 0, 0, 0, 0, 0, 0, 0, 1, 0x7F, 1, 2, 0, 0])
                .unwrap();
            closed(&mut s)
        });
        assert!(
            thread_result(peer).await,
            "mismatched socket type kept open"
        );
        drop(pull);
    });
}

/// A legacy peer must never get past the greeting with a socket that
/// authenticates.
#[test]
fn legacy_peers_cannot_downgrade_security() {
    let cases: Vec<(&str, SocketOptions)> = vec![
        (
            "PLAIN",
            opts().with_plain_server(true).with_zap_domain("global"),
        ),
        ("NULL with ZAP", opts().with_zap_domain("global")),
        (
            "CURVE",
            opts()
                .with_curve_server(true)
                .with_curve_keypair([0x11; 32], [0x22; 32]),
        ),
    ];
    for (name, options) in cases {
        for v1 in [false, true] {
            rt().block_on(async {
                let mut pull = PullSocket::with_options(options.clone());
                let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
                let peer = thread::spawn(move || {
                    let mut s = connect(&ep);
                    if v1 {
                        s.write_all(&[1, 0]).unwrap();
                    } else {
                        s.write_all(&[0xFF, 0, 0, 0, 0, 0, 0, 0, 1, 0x7F, 1, 8, 0, 0])
                            .unwrap();
                    }
                    s.write_all(&v2_frame(false, b"smuggled")).unwrap();
                    closed(&mut s)
                });
                assert!(thread_result(peer).await, "{name} accepted a legacy peer");
                let got = timeout(Duration::from_millis(100), pull.recv_owned()).await;
                assert!(got.is_err(), "{name} delivered a legacy peer's message");
            });
        }
    }
}

/// Join a blocking peer thread without stalling the runtime.
async fn thread_result<T: Send + 'static>(t: thread::JoinHandle<T>) -> T {
    while !t.is_finished() {
        sleep(Duration::from_millis(5)).await;
    }
    t.join().unwrap()
}
