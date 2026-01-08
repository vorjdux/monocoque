#[cfg(feature = "runtime")]
use monocoque_zmtp::RouterSocket;
use bytes::Bytes;
use std::thread;

#[cfg(feature = "runtime")]
#[test]
fn test_router_explicit_routing() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:5558").await.unwrap();
            ready_tx.send(()).unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            
            // Create ROUTER socket
            let router = RouterSocket::new(stream);

            // Receive message with identity envelope
            let msg = router.recv().await.unwrap();
                    tracing::info!("[Router] Received from: {:?}, body: {:?}", 
                                   std::str::from_utf8(&msg[0]).unwrap_or("???"),
                                   std::str::from_utf8(&msg[1]).unwrap_or("???"));
            
            // Verify identity and message
            assert_eq!(&msg[0][..], b"CLIENT_A");
            assert_eq!(&msg[1][..], b"Hello");

            // Send reply to specific peer (identity + body)
            router.send(vec![
                msg[0].clone(), // CLIENT_A identity
                Bytes::from_static(b"World"),
            ]).await.unwrap();
        });
    });

    ready_rx.recv().unwrap();

    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"CLIENT_A").unwrap();
    dealer.connect("tcp://127.0.0.1:5558").unwrap();

    dealer.send("Hello", 0).unwrap();

    let msg = dealer.recv_string(0).unwrap().unwrap();
    assert_eq!(msg, "World");
}

#[cfg(not(feature = "runtime"))]
#[test]
fn test_router_explicit_routing() {
    tracing::info!("Skipping interop_router test - requires 'runtime' feature");
}