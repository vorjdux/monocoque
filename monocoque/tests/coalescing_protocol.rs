//! Write coalescing batches the application's messages. What the protocol
//! itself sends must not wait for a batch to fill.
//!
//! It used to: a heartbeat PING or PONG, a subscription and a STREAM close
//! were all held like messages, and `flush()` did not release the commands
//! because they are not counted as messages.

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{
    Msg, PubSocket, PullSocket, PushSocket, SocketEvent, SocketOptions, StreamSocket, SubSocket,
};
use std::io::Read;
use std::time::Duration;

fn coalescing() -> SocketOptions {
    SocketOptions::default()
        .with_write_coalescing(true)
        .with_recv_timeout(Duration::from_secs(5))
}

/// Both ends coalesce and heartbeat, and nothing is sent. Each has to get
/// its PINGs out and its PONGs back, or it times the other out.
#[test]
fn heartbeats_are_not_held_by_coalescing() {
    LocalRuntime::new().unwrap().block_on(async {
        let beat = Duration::from_millis(100);
        let mut pull = PullSocket::with_options(coalescing().with_heartbeat_ivl(beat));
        let pull_events = pull.monitor();
        let endpoint = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::with_options(coalescing().with_heartbeat_ivl(beat));
        let push_events = push.monitor();
        push.connect(&endpoint).await.unwrap();
        while push.peer_count() == 0 || pull.peer_count() == 0 {
            monocoque::rt::sleep(Duration::from_millis(5)).await;
        }

        monocoque::rt::sleep(Duration::from_millis(1500)).await;

        for (side, events) in [("PULL", pull_events), ("PUSH", push_events)] {
            let dropped = events
                .try_iter()
                .filter(|e| matches!(e, SocketEvent::Disconnected(_)))
                .count();
            assert_eq!(dropped, 0, "{side} lost its idle connection");
        }
    });
}

/// A coalescing SUB's subscription reaches the publisher without a flush.
#[test]
fn a_subscription_is_not_held_by_coalescing() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut publisher = PubSocket::new();
        let endpoint = publisher.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut subscriber = SubSocket::with_options(coalescing());
        subscriber.connect(&endpoint).await.unwrap();
        subscriber.subscribe("").await.unwrap();

        let mut delivered = 0;
        for _ in 0..500 {
            delivered = publisher.send("news").await.unwrap();
            if delivered > 0 {
                break;
            }
            monocoque::rt::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(delivered, 1, "the publisher never saw the subscription");
        assert_eq!(subscriber.recv().await.unwrap(), [b"news"]);
    });
}

/// A coalescing STREAM socket writes what is queued for a connection before
/// it closes it.
#[test]
fn a_stream_close_does_not_drop_coalesced_data() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut server = StreamSocket::with_options(coalescing());
        let endpoint = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client =
            std::net::TcpStream::connect(endpoint.trim_start_matches("tcp://")).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let hello = server.recv_owned().await.unwrap();
        let identity = hello.frame_bytes(0).unwrap();
        server
            .send(Msg::from([
                identity.clone(),
                Bytes::from_static(b"last words"),
            ]))
            .await
            .unwrap();
        server
            .send(Msg::from([identity, Bytes::new()]))
            .await
            .unwrap();

        // Read on another thread: the server's writes need this one.
        let reader = std::thread::spawn(move || {
            let mut got = Vec::new();
            client.read_to_end(&mut got).map(|_| got)
        });
        while !reader.is_finished() {
            monocoque::rt::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(reader.join().unwrap().unwrap(), b"last words");
    });
}
