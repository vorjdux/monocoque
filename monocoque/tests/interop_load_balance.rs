use bytes::Bytes;
use monocoque::zmq::RouterSocket;
use std::thread;
use std::time::Duration;

#[test]
fn test_router_load_balancer_basic() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::net::SocketAddr>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    thread::spawn(move || {
        monocoque::rt::LocalRuntime::new().unwrap().block_on(async {
            let listener = monocoque::rt::TcpListener::bind("127.0.0.1:0")
                .await
                .unwrap();
            let local_addr = listener.local_addr().unwrap();
            ready_tx.send(local_addr).unwrap();

            let (stream, _) = listener.accept().await.unwrap();

            let mut router = RouterSocket::from_tcp(stream).await.unwrap();

            let msg = router.recv().await.unwrap().unwrap();

            router
                .send(vec![msg[0].clone(), Bytes::from("Response from Router")])
                .await
                .unwrap();

            result_tx.send(Ok(())).unwrap();
        });
    });

    let local_addr = ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"WORKER_1").unwrap();
    dealer.connect(&format!("tcp://{local_addr}")).unwrap();

    dealer.send("Task from worker", 0).unwrap();

    dealer.set_rcvtimeo(5000).unwrap();
    let response = dealer.recv_string(0).unwrap().unwrap();
    assert_eq!(response, "Response from Router");

    result_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
}
