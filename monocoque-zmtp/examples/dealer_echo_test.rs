use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_zmtp::dealer::DealerSocket;

fn main() {
    compio::runtime::Runtime::new().unwrap().block_on(async {
        println!("=== Monocoque DEALER Socket Example ===\n");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Start echo server
        compio::runtime::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut dealer = DealerSocket::new(stream).await.unwrap();
            println!("[Server] Connected");

            for i in 0..5 {
                if let Ok(Some(msg)) = dealer.recv().await {
                    println!("[Server] Received message {}: {:?}", i + 1, msg);
                    dealer.send(msg).await.ok();
                }
            }
        })
        .detach();

        compio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Connect client
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut dealer = DealerSocket::new(stream).await.unwrap();
        println!("[Client] Connected\n");

        // Send and receive messages
        for i in 0..5 {
            let msg = vec![Bytes::from(format!("Message {}", i + 1))];
            dealer.send(msg.clone()).await.unwrap();
            println!("[Client] Sent: {:?}", msg);

            if let Ok(Some(reply)) = dealer.recv().await {
                println!("[Client] Received echo: {:?}\n", reply);
            }
        }

        println!("✅ DEALER socket test complete");
    });
}
