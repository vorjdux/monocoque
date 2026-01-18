use monocoque::zmq::DealerSocket;
use bytes::Bytes;
use std::thread;

// TODO: These interop tests hang due to compio runtime not exiting cleanly in test harness
// They work fine when run manually or in examples. Need to investigate test lifecycle.
#[test]
#[ignore = "compio runtime lifecycle issues in test harness"]
fn test_interop_pair() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    
    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:5556").await.unwrap();
            ready_tx.send(()).unwrap();
            
            let (stream, _) = listener.accept().await.unwrap();
            
            // Create DEALER socket (works as PAIR for point-to-point)
            let mut dealer = DealerSocket::from_tcp(stream).await.unwrap();

            // Echo server logic
            let msg = dealer.recv().await.unwrap();
            assert_eq!(&msg[0][..], b"Ping");
            
            dealer.send(vec![Bytes::from_static(b"Pong")]).await.unwrap();
            
            drop(dealer);
        });
    });

    ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let sock = ctx.socket(zmq::PAIR).unwrap();
    sock.connect("tcp://127.0.0.1:5556").unwrap();
    
    sock.send("Ping", 0).unwrap();
    let msg = sock.recv_string(0).unwrap().unwrap();
    assert_eq!(msg, "Pong");
}

