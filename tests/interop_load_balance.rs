#[cfg(feature = "runtime")]
use monocoque_zmtp::RouterSocket;
use bytes::Bytes;
use std::thread;

#[cfg(feature = "runtime")]
#[test]
fn test_router_load_balancer_basic() {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    // Router thread
    thread::spawn(move || {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = compio::net::TcpListener::bind("127.0.0.1:5559").await.unwrap();
            ready_tx.send(()).unwrap();

            // Accept first connection (DEALER client)
            let (stream, _) = listener.accept().await.unwrap();
            
            // Create ROUTER socket
            let router = RouterSocket::new(stream);

            // Receive message from dealer
            let msg = router.recv().await.unwrap();
            println!("[Router] Received from: {:?}", 
                     std::str::from_utf8(&msg[0]).unwrap_or("???"));

            // Send response to specific peer
            router.send(vec![
                msg[0].clone(), // Return to sender
                Bytes::from("Response from Router"),
            ]).await.unwrap();
        });
    });

    ready_rx.recv().unwrap();

    // Client using libzmq DEALER
    let ctx = zmq::Context::new();
    let dealer = ctx.socket(zmq::DEALER).unwrap();
    dealer.set_identity(b"WORKER_1").unwrap();
    dealer.connect("tcp://127.0.0.1:5559").unwrap();

    // Send request
    dealer.send("Task from worker", 0).unwrap();

    // Receive response
    let response = dealer.recv_string(0).unwrap().unwrap();
    assert_eq!(response, "Response from Router");
}

#[cfg(not(feature = "runtime"))]
#[test]
fn test_router_load_balancer_basic() {
    println!("Skipping interop_load_balance test - requires 'runtime' feature");
}