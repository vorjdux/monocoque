//! `ZMQ_CONFLATE`: a receiving socket keeps only the newest message from each
//! peer, and its receive HWM no longer applies.

use bytes::Bytes;
use monocoque_core::msg::Msg;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{LocalRuntime, sleep, timeout};
use monocoque_zmtp::sock::kinds::*;
use std::time::Duration;

const N: u32 = 500;

fn conflating() -> SocketOptions {
    SocketOptions::default()
        .with_conflate(true)
        .with_recv_hwm(10)
        .with_recv_timeout(Duration::from_secs(5))
}

fn numbered(i: u32) -> Msg {
    Msg::from(Bytes::copy_from_slice(&i.to_be_bytes()))
}

fn number(m: &Msg) -> u32 {
    u32::from_be_bytes(m.first().unwrap().try_into().unwrap())
}

async fn pull_keeps_the_newest(endpoint: &str) {
    let mut pull = PullSocket::with_options(conflating());
    let ep = pull.bind(endpoint).await.unwrap();
    let mut push = PushSocket::new();
    push.connect(&ep).await.unwrap();
    for i in 0..N {
        push.send(numbered(i)).await.unwrap();
    }
    // Let everything land before the first receive.
    sleep(Duration::from_millis(300)).await;
    let m = pull.recv_owned().await.unwrap();
    assert_eq!(number(&m), N - 1, "conflate kept an old message");
    let more = timeout(Duration::from_millis(100), pull.recv_owned()).await;
    assert!(more.is_err(), "conflate kept more than one message");

    // Still live afterwards.
    push.send(numbered(N)).await.unwrap();
    assert_eq!(number(&pull.recv_owned().await.unwrap()), N);
}

#[test]
fn pull_conflates_over_tcp() {
    LocalRuntime::new()
        .unwrap()
        .block_on(pull_keeps_the_newest("tcp://127.0.0.1:0"));
}

#[test]
fn pull_conflates_over_inproc() {
    LocalRuntime::new()
        .unwrap()
        .block_on(pull_keeps_the_newest("inproc://conflate-pull"));
}

#[test]
fn sub_conflates() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut publisher = PubSocket::new();
        let ep = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut sub = SubSocket::with_options(conflating());
        sub.subscribe(b"").await.unwrap();
        sub.connect(&ep).await.unwrap();
        while publisher.send(numbered(0)).await.unwrap() == 0 {
            sleep(Duration::from_millis(5)).await;
        }
        for i in 1..N {
            publisher.send(numbered(i)).await.unwrap();
        }
        sleep(Duration::from_millis(300)).await;
        assert_eq!(number(&sub.recv_owned().await.unwrap()), N - 1);
        let more = timeout(Duration::from_millis(100), sub.recv_owned()).await;
        assert!(more.is_err(), "conflate kept more than one message");
    });
}

/// Without conflate the same traffic all arrives, in order.
#[test]
fn without_conflate_every_message_arrives() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::new();
        push.connect(&ep).await.unwrap();
        for i in 0..N {
            push.send(numbered(i)).await.unwrap();
        }
        for i in 0..N {
            assert_eq!(number(&pull.recv_owned().await.unwrap()), i);
        }
    });
}
