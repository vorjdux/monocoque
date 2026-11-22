//! Sends that put nothing on the wire.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, MonocoqueError, Msg, RouterSocket, SocketOptions};
use std::time::{Duration, Instant};

fn opts() -> SocketOptions {
    SocketOptions::default()
        .with_recv_timeout(Duration::from_secs(5))
        .with_linger(Some(Duration::from_secs(20)))
}

/// A message with no frames encodes to nothing. It used to be counted as
/// queued all the same, so `close()` waited its whole linger for bytes that
/// were never going to be written.
#[test]
fn an_empty_message_does_not_hold_up_close() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut server = DealerSocket::with_options(opts());
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client = DealerSocket::with_options(opts());
        client.connect(&ep).await.unwrap();
        client.send(Msg::from("first")).await.unwrap();
        assert_eq!(server.recv().await.unwrap(), [b"first"]);

        client.send(Msg::new()).await.unwrap();
        let began = Instant::now();
        client.close().await.unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "close waited {:?} on a message that was never encoded",
            began.elapsed()
        );
    });
}

/// The same for a ROUTER message that is only its identity frame: the
/// identity picks the peer and nothing is left to send.
#[test]
fn a_router_message_that_is_only_an_identity_does_not_hold_up_close() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut router = RouterSocket::with_options(opts());
        let ep = router.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut dealer = DealerSocket::with_options(opts());
        dealer.connect(&ep).await.unwrap();
        dealer.send(Msg::from("hello")).await.unwrap();

        let mut request = router.recv_owned().await.unwrap();
        let identity = request.pop_front().expect("identity frame");
        router.send(Msg::from(identity)).await.unwrap();

        let began = Instant::now();
        router.close().await.unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "close waited {:?} on a message that was never encoded",
            began.elapsed()
        );
    });
}

/// `disconnect` while the handshake is still running closes that connection
/// at once. It used to stay open until the handshake finished or timed out,
/// 30 seconds by default.
#[test]
fn disconnect_during_a_handshake_closes_the_connection() {
    use std::io::Read;

    // A peer that accepts and then says nothing: the handshake cannot end.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("tcp://{}", listener.local_addr().unwrap());
    let silent_peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let began = Instant::now();
        let mut sink = Vec::new();
        // Returns when the other side closes, or fails when the read times out.
        stream.read_to_end(&mut sink).map(|_| began.elapsed())
    });

    LocalRuntime::new().unwrap().block_on(async {
        let mut client = DealerSocket::with_options(opts());
        client.connect(&endpoint).await.unwrap();
        // Long enough for the dial to land and the greeting to go out.
        monocoque::rt::sleep(Duration::from_millis(200)).await;
        client.disconnect(&endpoint).unwrap();
        assert!(client.disconnect(&endpoint).is_err(), "it is gone");
        // Keep the socket alive: only the disconnect may close the stream.
        monocoque::rt::sleep(Duration::from_secs(1)).await;
        drop(client);
    });

    let closed_after = silent_peer.join().unwrap().expect("the stream was closed");
    assert!(
        closed_after < Duration::from_millis(900),
        "the connection stayed open for {closed_after:?} after the disconnect"
    );
}

/// A routing ID libzmq would refuse (empty, or longer than 255 bytes) never
/// reaches a peer: the socket refuses to bind or connect with it.
#[test]
fn a_routing_id_of_the_wrong_length_is_refused() {
    LocalRuntime::new().unwrap().block_on(async {
        for len in [0, 256] {
            let options = opts().with_routing_id(vec![1u8; len].into());
            let mut dealer = DealerSocket::with_options(options.clone());
            let connected = dealer.connect("tcp://127.0.0.1:5555").await;
            assert!(
                matches!(connected, Err(MonocoqueError::InvalidRoutingId)),
                "connect with a {len}-byte routing ID gave {connected:?}"
            );
            let mut dealer = DealerSocket::with_options(options);
            let bound = dealer.bind("tcp://127.0.0.1:0").await;
            assert!(
                matches!(bound, Err(MonocoqueError::InvalidRoutingId)),
                "bind with a {len}-byte routing ID gave {bound:?}"
            );
        }

        let mut dealer = DealerSocket::with_options(opts().with_routing_id(vec![1u8; 255].into()));
        dealer.bind("tcp://127.0.0.1:0").await.unwrap();
    });
}

/// An endpoint written with a host name is disconnected by the same text.
#[test]
fn disconnect_takes_the_endpoint_as_it_was_given() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut server = DealerSocket::with_options(opts());
        let bound = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let port = bound.rsplit(':').next().unwrap();
        let by_name = format!("tcp://localhost:{port}");

        let mut client = DealerSocket::with_options(opts());
        client.connect(&by_name).await.unwrap();
        while client.peer_count() == 0 {
            monocoque::rt::sleep(Duration::from_millis(5)).await;
        }
        client.disconnect(&by_name).unwrap();
        assert_eq!(client.peer_count(), 0);
    });
}
