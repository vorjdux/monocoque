use bytes::Bytes;
use monocoque::zmq::PairSocket;
use std::thread;
use std::time::Duration;

#[test]
fn test_interop_pair() {
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

            let mut pair = PairSocket::from_tcp(stream).await.unwrap();

            let msg = pair.recv().await.unwrap().unwrap();
            if msg[0] != b"Ping"[..] {
                result_tx
                    .send(Err(format!("Expected Ping, got {:?}", msg[0])))
                    .unwrap();
                return;
            }

            pair.send(vec![Bytes::from_static(b"Pong")]).await.unwrap();
            result_tx.send(Ok(())).unwrap();
        });
    });

    let local_addr = ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let sock = ctx.socket(zmq::PAIR).unwrap();
    sock.connect(&format!("tcp://{local_addr}")).unwrap();

    sock.send("Ping", 0).unwrap();
    sock.set_rcvtimeo(5000).unwrap();
    let msg = sock.recv_string(0).unwrap().unwrap();
    assert_eq!(msg, "Pong");

    result_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
}
