use monocoque::zmq::PubSocket;
use bytes::Bytes;
use std::thread;
use std::time::Duration;

#[test]
fn test_pubsub_basic() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    // Subscriber signals when it has connected and subscribed, so publisher
    // knows it's safe to broadcast without relying on a fixed sleep duration.
    let (sub_ready_tx, sub_ready_rx) = std::sync::mpsc::channel::<()>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
            let local_addr = pub_socket.local_addr().unwrap();
            ready_tx.send(local_addr).unwrap();

            pub_socket.accept_subscriber().await.unwrap();

            // Wait for subscriber to signal it is subscribed before broadcasting.
            // Use a blocking recv — the publisher has nothing else to do while waiting,
            // and compio::time::sleep interferes with the handshake timer state.
            sub_ready_rx.recv().unwrap();

            pub_socket.send(vec![
                Bytes::from("topic.test"),
                Bytes::from("Hello PubSub!"),
            ]).await.unwrap();

            result_tx.send(Ok(())).unwrap();
        });
    });

    let local_addr = ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let sub = ctx.socket(zmq::SUB).unwrap();
    sub.connect(&format!("tcp://{}", local_addr)).unwrap();
    sub.set_subscribe(b"topic").unwrap();
    sub.set_rcvtimeo(5000).unwrap();

    // Give libzmq time to send the subscription frame to the PUB socket
    thread::sleep(Duration::from_millis(50));
    sub_ready_tx.send(()).unwrap();

    let topic = sub.recv_string(0).unwrap().unwrap();
    let body = sub.recv_string(0).unwrap().unwrap();

    assert_eq!(topic, "topic.test");
    assert_eq!(body, "Hello PubSub!");

    result_rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
}
