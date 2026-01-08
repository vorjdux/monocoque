//! Ultra-minimal latency test - just measure the round-trip with BOTH direct implementations

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_zmtp::rep::RepSocket;
use monocoque_zmtp::req::ReqSocket;
use std::time::Instant;

fn main() {
    compio::runtime::Runtime::new().unwrap().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Start echo server with DIRECT implementation
        let server = compio::runtime::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut rep = RepSocket::new(stream).await.unwrap();
            
            // Echo exactly 10 messages
            for _ in 0..10 {
                if let Ok(Some(msg)) = rep.recv().await {
                    rep.send(msg).await.ok();
                } else {
                    break;
                }
            }
        });

        compio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Connect client with DIRECT implementation
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut socket = ReqSocket::new(stream).await.unwrap();
        let payload = Bytes::from(vec![0u8; 64]);

        println!("Measuring 10 round-trips (BOTH DIRECT)...");

        let start = Instant::now();
        for i in 0..10 {
            socket.send(vec![payload.clone()]).await.unwrap();
            socket.recv().await.unwrap();
            println!("  Round-trip {} done", i + 1);
        }
        let elapsed = start.elapsed();

        server.await;

        println!("\nTotal: {:?}", elapsed);
        println!("Per message: {:?}", elapsed / 10);
        println!("Throughput: {:.0} msgs/sec", 10.0 / elapsed.as_secs_f64());
    });
}
