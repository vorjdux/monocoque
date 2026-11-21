//! A coalescing sender that crosses its threshold and then goes quiet.
//!
//! A threshold release writes whole TCP segments and keeps the bytes left
//! over so they ride with the next batch. If no next batch comes, they must
//! not wait for a `flush()` the sender has no reason to call: every message
//! queued when the threshold released reaches the peer once the sending task
//! yields.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, PullSocket, PushSocket, SocketOptions};
use std::time::Duration;

/// Send `count` messages of `size` bytes with no flush; all must arrive.
fn all_arrive(count: usize, size: usize) {
    LocalRuntime::new().unwrap().block_on(async move {
        let mut pull = PullSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(1)),
        );
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push =
            PushSocket::with_options(SocketOptions::default().with_write_coalescing(true));
        push.connect(&ep).await.unwrap();
        let payload = Msg::from(vec![5u8; size]);
        let sender = async {
            for _ in 0..count {
                push.send(payload.clone()).await.unwrap();
            }
        };
        let receiver = async {
            for i in 0..count {
                let msg = pull.recv().await.unwrap_or_else(|e| {
                    panic!("{i} of {count} x {size} B arrived, then nothing: {e}")
                });
                assert_eq!(msg.first().map(<[u8]>::len), Some(size));
            }
        };
        futures::join!(sender, receiver);
    });
}

#[test]
fn one_message_larger_than_a_segment() {
    all_arrive(1, 100 * 1024);
}

#[test]
fn one_message_of_several_segments() {
    all_arrive(1, 256 * 1024);
}

#[test]
fn two_messages_of_a_segment_each() {
    all_arrive(2, 64 * 1024);
}

#[test]
fn small_messages_just_past_the_threshold() {
    all_arrive(993, 64);
}

#[test]
fn medium_messages_just_past_the_threshold() {
    all_arrive(64, 1024);
}
