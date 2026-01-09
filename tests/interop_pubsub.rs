#[cfg(feature = "runtime")]
use monocoque_zmtp::{PubSocket, SubSocket};
use bytes::Bytes;
use std::thread;
use std::time::Duration;

#[cfg(feature = "runtime")]
#[test]
fn test_pubsub_basic() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    // Publisher thread
    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:5560").await.unwrap();
            ready_tx.send(()).unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            
            // Create PUB socket
            let mut pub_socket = PubSocket::new(stream);

            // Give subscriber time to connect and subscribe
            compio::time::sleep(Duration::from_millis(100)).await;

            // Publish message
            pub_socket.send(vec![
                Bytes::from("topic.test"),
                Bytes::from("Hello PubSub!"),
            ]).await.unwrap();
        });
    });

    ready_rx.recv().unwrap();

    // Subscriber using libzmq
    let ctx = zmq::Context::new();
    let sub = ctx.socket(zmq::SUB).unwrap();
    sub.connect("tcp://127.0.0.1:5560").unwrap();
    sub.set_subscribe(b"topic").unwrap();

    // Give time for connection
    thread::sleep(Duration::from_millis(50));

    // Receive message
    let topic = sub.recv_string(0).unwrap().unwrap();
    let body = sub.recv_string(0).unwrap().unwrap();
    
    assert_eq!(topic, "topic.test");
    assert_eq!(body, "Hello PubSub!");
}

#[cfg(not(feature = "runtime"))]
#[test]
fn test_pubsub_basic() {
    println!("Skipping interop_pubsub test - requires 'runtime' feature");
}