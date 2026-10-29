//! The draft socket types. The installed libzmq is built without draft
//! support, so these run monocoque against itself, over TCP and inproc.
#![cfg(feature = "draft")]

use bytes::Bytes;
use monocoque_core::error::MonocoqueError;
use monocoque_core::msg::Msg;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{LocalRuntime, sleep, timeout};
use monocoque_zmtp::sock::kinds::DealerSocket;
use monocoque_zmtp::sock::kinds::draft::*;
use std::time::Duration;

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

fn two_frames() -> Msg {
    Msg::from([Bytes::from_static(b"a"), Bytes::from_static(b"b")])
}

async fn client_server(endpoint: &str) {
    let mut server = ServerSocket::with_options(opts());
    let ep = server.bind(endpoint).await.unwrap();
    let mut a = ClientSocket::with_options(opts());
    let mut b = ClientSocket::with_options(opts());
    a.connect(&ep).await.unwrap();
    b.connect(&ep).await.unwrap();

    a.send("from a").await.unwrap();
    let (ra, m) = server.recv_owned().await.unwrap();
    assert_eq!(m.first(), Some(&b"from a"[..]));
    b.send("from b").await.unwrap();
    let (rb, m) = server.recv_owned().await.unwrap();
    assert_eq!(m.first(), Some(&b"from b"[..]));
    assert_ne!(ra, 0);
    assert_ne!(ra, rb, "each peer gets its own routing id");

    server.send(rb, "to b").await.unwrap();
    server.send(ra, "to a").await.unwrap();
    assert_eq!(a.recv_owned().await.unwrap().first(), Some(&b"to a"[..]));
    assert_eq!(b.recv_owned().await.unwrap().first(), Some(&b"to b"[..]));

    let unknown = ra.wrapping_add(rb).wrapping_add(7);
    assert!(matches!(
        server.send(unknown, "x").await,
        Err(MonocoqueError::HostUnreachable)
    ));
    assert!(matches!(
        a.send(two_frames()).await,
        Err(MonocoqueError::InvalidState(_))
    ));
    assert!(matches!(
        server.send(ra, two_frames()).await,
        Err(MonocoqueError::InvalidState(_))
    ));
}

#[test]
fn client_server_over_tcp() {
    LocalRuntime::new()
        .unwrap()
        .block_on(client_server("tcp://127.0.0.1:0"));
}

#[test]
fn client_server_over_inproc() {
    LocalRuntime::new()
        .unwrap()
        .block_on(client_server("inproc://draft-client-server"));
}

async fn radio_dish(endpoint: &str) {
    let mut radio = RadioSocket::new();
    let ep = radio.bind(endpoint).await.unwrap();
    let mut dish = DishSocket::with_options(opts());
    dish.join("weather").await.unwrap();
    dish.connect(&ep).await.unwrap();

    // Wait for the JOIN to land; nothing reaches a group nobody joined.
    while radio.send("weather", "probe").await.unwrap() == 0 {
        assert_eq!(radio.send("sports", "x").await.unwrap(), 0);
        sleep(Duration::from_millis(5)).await;
    }
    radio.send("weathe", "prefixes do not match").await.unwrap();
    radio.send("weather", "sunny").await.unwrap();
    let mut bodies = Vec::new();
    loop {
        let m = dish.recv_owned().await.unwrap();
        let frames: Vec<&[u8]> = m.frames().collect();
        assert_eq!(frames[0], b"weather", "DISH receives [group, body]");
        if frames[1] == b"sunny" {
            break;
        }
        bodies.push(frames[1].to_vec());
    }
    assert!(bodies.iter().all(|b| b == b"probe"));

    dish.leave("weather").await.unwrap();
    while radio.send("weather", "after leave").await.unwrap() != 0 {
        sleep(Duration::from_millis(5)).await;
    }
    let late = timeout(Duration::from_millis(100), dish.recv_owned()).await;
    assert!(
        late.is_err() || late.unwrap().unwrap().frames().nth(1) != Some(&b"after leave"[..]),
        "DISH received a group it left"
    );
    assert!(radio.send(vec![b'g'; MAX_GROUP + 1], "x").await.is_err());
}

#[test]
fn radio_dish_over_tcp() {
    LocalRuntime::new()
        .unwrap()
        .block_on(radio_dish("tcp://127.0.0.1:0"));
}

#[test]
fn radio_dish_over_inproc() {
    LocalRuntime::new()
        .unwrap()
        .block_on(radio_dish("inproc://draft-radio-dish"));
}

#[test]
fn scatter_gather_and_channel() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut gather = GatherSocket::with_options(opts());
        let ep = gather.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut scatter = ScatterSocket::new();
        scatter.connect(&ep).await.unwrap();
        for i in 0..10u8 {
            scatter.send(vec![i]).await.unwrap();
        }
        for i in 0..10u8 {
            assert_eq!(gather.recv_owned().await.unwrap().first(), Some(&[i][..]));
        }

        let mut c1 = ChannelSocket::with_options(opts());
        let ep = c1.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut c2 = ChannelSocket::with_options(opts());
        c2.connect(&ep).await.unwrap();
        c2.send("hi").await.unwrap();
        assert_eq!(c1.recv_owned().await.unwrap().first(), Some(&b"hi"[..]));
        c1.send("back").await.unwrap();
        assert_eq!(c2.recv_owned().await.unwrap().first(), Some(&b"back"[..]));
    });
}

/// A draft socket only talks to its own partner type.
#[test]
fn draft_types_refuse_classic_peers() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut server = ServerSocket::with_options(opts());
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer = DealerSocket::new();
        dealer.connect(&ep).await.unwrap();
        // The DEALER never completes a handshake, so its message stays queued.
        let _ = timeout(Duration::from_millis(200), dealer.send("x")).await;
        let got = timeout(Duration::from_millis(200), server.recv_owned()).await;
        assert!(got.is_err(), "SERVER accepted a DEALER");
    });
}
