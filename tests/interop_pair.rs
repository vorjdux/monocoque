#[cfg(feature = "runtime")]
use monocoque_zmtp::DealerSocket;
use bytes::Bytes;
use std::thread;

#[cfg(feature = "runtime")]
#[test]
fn test_interop_pair() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    
    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:5556").await.unwrap();
            ready_tx.send(()).unwrap();
            
            let (stream, _) = listener.accept().await.unwrap();
            
            // Create DEALER socket (works as PAIR for point-to-point)
            let dealer = DealerSocket::new(stream);

            // Echo server logic
            let msg = dealer.recv().await.unwrap();
            assert_eq!(&msg[0][..], b"Ping");
            
            dealer.send(vec![Bytes::from_static(b"Pong")]).await.unwrap();
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

#[cfg(not(feature = "runtime"))]
#[test]
fn test_interop_pair() {
    // Skip test if runtime feature not enabled
    println!("Skipping interop_pair test - requires 'runtime' feature");
}