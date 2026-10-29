//! Message metadata (`zmq_msg_gets`): connection properties reach every
//! received message.
//!
//! The ZAP endpoint is a process-global inproc name, so this file holds the
//! only ZAP server bound in its process.

use bytes::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, PullSocket, PushSocket, RouterSocket, SocketOptions};
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse};
use monocoque_zmtp::security::zap_handler::{ZapHandler, spawn_zap_server};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

#[test]
fn tcp_messages_carry_socket_type_peer_address_identity_and_custom_properties() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::with_options(
            SocketOptions::default()
                .with_routing_id(Bytes::from_static(b"pusher-7"))
                .with_metadata("X-Service", "billing"),
        );
        push.connect(&ep).await.unwrap();
        push.send("one").await.unwrap();
        push.send("two").await.unwrap();

        let msg = pull.recv().await.unwrap();
        assert_eq!(msg, [b"one"]);
        assert_eq!(msg.gets("Socket-Type"), Some("PUSH"));
        assert_eq!(msg.gets("socket-type"), Some("PUSH"), "names ignore case");
        assert_eq!(msg.gets("Peer-Address"), Some("127.0.0.1"));
        assert_eq!(msg.gets("Identity"), Some("pusher-7"));
        assert_eq!(msg.gets("Routing-Id"), Some("pusher-7"));
        assert_eq!(msg.gets("X-Service"), Some("billing"));
        assert_eq!(msg.gets("User-Id"), None, "no ZAP, no user id");
        assert_eq!(msg.gets("X-Missing"), None);

        // Every later message from the connection shares the same metadata,
        // and the socket keeps it for messages taken out as owned.
        let owned = pull.recv().await.unwrap().take();
        assert_eq!(owned, [b"two"]);
        let meta = pull.last_metadata().expect("metadata of the last message");
        assert_eq!(meta.gets("X-Service"), Some("billing"));
        assert_eq!(
            pull.peer_property("Socket-Type").as_deref(),
            Some(&b"PUSH"[..])
        );
    });
}

#[test]
fn inproc_messages_carry_socket_type_and_custom_properties() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut router = RouterSocket::with_options(opts());
        router.bind("inproc://msg-metadata").await.unwrap();
        let mut dealer = DealerSocket::with_options(
            SocketOptions::default()
                .with_routing_id(Bytes::from_static(b"d1"))
                .with_metadata("X-Shard", "3"),
        );
        dealer.connect("inproc://msg-metadata").await.unwrap();
        dealer.send("hi").await.unwrap();
        let msg = router.recv().await.unwrap();
        assert_eq!(msg.gets("Socket-Type"), Some("DEALER"));
        assert_eq!(msg.gets("Identity"), Some("d1"));
        assert_eq!(msg.gets("X-Shard"), Some("3"));
        assert_eq!(msg.gets("Peer-Address"), None);
    });
}

struct UserIdHandler;

impl ZapHandler for UserIdHandler {
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        let mut reply = ZapResponse::success(request.request_id.clone(), "uid-alice");
        reply
            .metadata
            .insert("X-Role".to_string(), "admin".to_string());
        reply
    }
}

#[test]
fn plain_with_zap_exposes_the_user_id_and_zap_metadata() {
    LocalRuntime::new().unwrap().block_on(async {
        spawn_zap_server(Arc::new(UserIdHandler)).unwrap();
        let timeout = Duration::from_secs(5);
        let mut server = DealerSocket::with_options(
            opts()
                .with_plain_server(true)
                .with_zap_domain("meta")
                .with_handshake_timeout(timeout),
        );
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client = DealerSocket::with_options(
            opts()
                .with_plain_credentials("alice", "secret")
                .with_metadata("X-Client", "cli")
                .with_handshake_timeout(timeout),
        );
        client.connect(&ep).await.unwrap();
        client.send("authed").await.unwrap();

        let msg = server.recv().await.unwrap();
        assert_eq!(msg, [b"authed"]);
        assert_eq!(msg.gets("User-Id"), Some("uid-alice"));
        assert_eq!(msg.gets("X-Role"), Some("admin"));
        // The PLAIN client's INITIATE carried its custom property too.
        assert_eq!(msg.gets("X-Client"), Some("cli"));
        assert_eq!(msg.gets("Socket-Type"), Some("DEALER"));
    });
}

/// A real libzmq peer: we read the properties its READY announced, and it
/// reads the custom property ours announced.
#[test]
fn interop_metadata_with_libzmq() {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let server = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut router = RouterSocket::with_options(opts());
            ep_tx.send(router.bind("tcp://127.0.0.1:0").await.unwrap()).unwrap();
            let msg = router.recv().await.unwrap();
            assert_eq!(msg.get(1), Some(&b"from libzmq"[..]));
            assert_eq!(msg.gets("Socket-Type"), Some("DEALER"));
            // libzmq announces an Identity only from REQ, DEALER and ROUTER.
            assert_eq!(msg.gets("Identity"), Some("zmq-dealer"));
            assert_eq!(msg.gets("Peer-Address"), Some("127.0.0.1"));
        });
    });
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"zmq-dealer").unwrap();
    dealer.connect(&ep_rx.recv().unwrap()).unwrap();
    dealer.send("from libzmq", 0).unwrap();
    server.join().unwrap();

    let pull = ctx.socket(zmq::PULL).unwrap();
    pull.set_rcvtimeo(5000).unwrap();
    pull.bind("tcp://127.0.0.1:*").unwrap();
    let endpoint = pull.get_last_endpoint().unwrap().unwrap();
    let client = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut push = PushSocket::with_options(
                SocketOptions::default().with_metadata("X-Service", "billing"),
            );
            push.connect(&endpoint).await.unwrap();
            push.send("from monocoque").await.unwrap();
            push.close().await.unwrap();
        });
    });
    let mut msg = zmq::Message::new();
    pull.recv(&mut msg, 0).expect("libzmq PULL got nothing");
    assert_eq!(&*msg, b"from monocoque");
    assert_eq!(msg.gets("Socket-Type"), Some("PUSH"));
    assert_eq!(msg.gets("X-Service"), Some("billing"));
    client.join().unwrap();
}
