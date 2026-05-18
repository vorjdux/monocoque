use bytes::Bytes;
use monocoque::zmq::RouterSocket;
use std::thread;
use std::time::Duration;

#[test]
fn test_router_explicit_routing() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local_addr = listener.local_addr().unwrap();
            ready_tx.send(local_addr).unwrap();

            let (stream, _) = listener.accept().await.unwrap();

            let mut router = RouterSocket::from_tcp(stream).await.unwrap();

            let msg = router.recv().await.unwrap().unwrap();
            if msg[0] != b"CLIENT_A"[..] {
                result_tx
                    .send(Err(format!("Expected CLIENT_A identity, got {:?}", msg[0])))
                    .unwrap();
                return;
            }
            if msg[1] != b"Hello"[..] {
                result_tx
                    .send(Err(format!("Expected Hello, got {:?}", msg[1])))
                    .unwrap();
                return;
            }

            router
                .send(vec![msg[0].clone(), Bytes::from_static(b"World")])
                .await
                .unwrap();

            result_tx.send(Ok(())).unwrap();
        });
    });

    let local_addr = ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"CLIENT_A").unwrap();
    dealer.connect(&format!("tcp://{}", local_addr)).unwrap();

    dealer.send("Hello", 0).unwrap();

    dealer.set_rcvtimeo(5000).unwrap();
    let msg = dealer.recv_string(0).unwrap().unwrap();
    assert_eq!(msg, "World");

    result_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
}
