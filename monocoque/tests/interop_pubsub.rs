use bytes::Bytes;
use monocoque::zmq::PubSocket;
use std::thread;
use std::time::Duration;

#[test]
fn test_pubsub_basic() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    // Subscriber signals when it has connected and subscribed, so the publisher
    // knows it is safe to start broadcasting.
    let (sub_ready_tx, sub_ready_rx) = std::sync::mpsc::channel::<()>();
    // Main thread signals the publisher to wind down once it has received.
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let publisher = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new().unwrap().block_on(async {
            let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
            let local_addr = pub_socket.local_addr().unwrap();
            ready_tx.send(local_addr).unwrap();

            pub_socket.accept_subscriber().await.unwrap();

            // Wait for the subscriber to connect and issue its subscription.
            sub_ready_rx.recv().unwrap();

            // PUB/SUB is a slow joiner: the subscription frame may not be
            // registered on the PUB side yet, so the first broadcasts can be
            // dropped silently. Oversend until the subscriber confirms receipt
            // (via the stop channel) instead of racing the subscription window
            // with a single send. This keeps the test deterministic on both
            // runtime backends. A std sleep between sends (not monocoque::rt::sleep,
            // which would perturb the handshake timer state) paces the loop.
            loop {
                pub_socket
                    .send(vec![
                        Bytes::from("topic.test"),
                        Bytes::from("Hello PubSub!"),
                    ])
                    .await
                    .unwrap();
                // Stop once the subscriber has received, or if it went away.
                if !matches!(
                    stop_rx.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ) {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
    });

    let local_addr = ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let sub = ctx.socket(zmq::SUB).unwrap();
    sub.connect(&format!("tcp://{local_addr}")).unwrap();
    sub.set_subscribe(b"topic").unwrap();
    sub.set_rcvtimeo(5000).unwrap();

    // Give libzmq time to send the subscription frame to the PUB socket.
    thread::sleep(Duration::from_millis(50));
    sub_ready_tx.send(()).unwrap();

    // Frames alternate topic/body across the oversent stream, so the first two
    // frames received are one complete message.
    let topic = sub.recv_string(0).unwrap().unwrap();
    let body = sub.recv_string(0).unwrap().unwrap();

    assert_eq!(topic, "topic.test");
    assert_eq!(body, "Hello PubSub!");

    // Wind the publisher down and make sure it exits cleanly.
    let _ = stop_tx.send(());
    publisher.join().unwrap();
}
