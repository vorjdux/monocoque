//! A peer's PING names a TTL: "close this connection if you hear nothing
//! from me for this long" (RFC 37). That holds whether or not this side
//! sends heartbeats of its own, as it does in libzmq. It used to be looked
//! at only on a socket with a heartbeat interval set.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::PullSocket;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// A PING command with `ttl` tenths of a second and no context.
fn ping(ttl: u16) -> Vec<u8> {
    let mut frame = vec![0x04, 7, 4];
    frame.extend_from_slice(b"PING");
    frame.extend_from_slice(&ttl.to_be_bytes());
    frame
}

/// Do a ZMTP 3 NULL handshake as a PUSH socket.
fn handshake(s: &mut TcpStream) {
    let mut greeting = [0u8; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3;
    greeting[12..16].copy_from_slice(b"NULL");
    s.write_all(&greeting).unwrap();
    let mut peer = [0u8; 64];
    s.read_exact(&mut peer).unwrap();
    let mut ready = vec![5u8];
    ready.extend_from_slice(b"READY");
    ready.push(11);
    ready.extend_from_slice(b"Socket-Type");
    ready.extend_from_slice(&4u32.to_be_bytes());
    ready.extend_from_slice(b"PUSH");
    s.write_all(&[0x04, u8::try_from(ready.len()).unwrap()])
        .unwrap();
    s.write_all(&ready).unwrap();
}

/// A PULL socket with no heartbeat options, kept alive for `life`. Returns
/// its endpoint.
fn pull_socket_for(life: Duration) -> String {
    let (ep_tx, ep_rx) = mpsc::channel();
    thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pull = PullSocket::new();
            ep_tx
                .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            monocoque::rt::sleep(life).await;
        });
    });
    ep_rx.recv().unwrap()
}

/// How long until the socket closes the connection, or `None` if it is still
/// open after `wait`. `keep_alive` is sent every 100 ms meanwhile, if given.
fn closed_within(s: &mut TcpStream, wait: Duration, keep_alive: Option<&[u8]>) -> Option<Duration> {
    let began = Instant::now();
    s.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut buf = [0u8; 4096];
    while began.elapsed() < wait {
        match s.read(&mut buf) {
            Ok(0) => return Some(began.elapsed()),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if let Some(frame) = keep_alive {
                    s.write_all(frame).unwrap();
                }
            }
            Err(_) => return Some(began.elapsed()),
        }
    }
    None
}

fn connect(ep: &str) -> TcpStream {
    let mut s = TcpStream::connect(ep.strip_prefix("tcp://").unwrap()).unwrap();
    handshake(&mut s);
    s
}

#[test]
fn a_silent_peer_is_closed_at_the_ttl_it_named() {
    let ep = pull_socket_for(Duration::from_secs(5));
    let mut s = connect(&ep);
    s.write_all(&ping(3)).unwrap();
    let closed = closed_within(&mut s, Duration::from_secs(3), None)
        .expect("the socket kept a peer that went silent past its TTL");
    assert!(
        closed >= Duration::from_millis(250),
        "closed after {closed:?}, before the 300 ms TTL was up"
    );
}

#[test]
fn a_peer_that_keeps_talking_is_kept() {
    let ep = pull_socket_for(Duration::from_secs(5));
    let mut s = connect(&ep);
    s.write_all(&ping(3)).unwrap();
    let closed = closed_within(&mut s, Duration::from_millis(1500), Some(&ping(3)));
    assert_eq!(closed, None, "a peer inside its TTL was closed");
}

#[test]
fn a_ping_without_a_ttl_asks_for_nothing() {
    let ep = pull_socket_for(Duration::from_secs(5));
    let mut s = connect(&ep);
    s.write_all(&ping(0)).unwrap();
    let closed = closed_within(&mut s, Duration::from_millis(1500), None);
    assert_eq!(closed, None, "a silent peer that named no TTL was closed");
}
