//! Simple direct performance test - BOTH DIRECT IMPLEMENTATIONS

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_zmtp::rep::RepSocket;
use monocoque_zmtp::req::ReqSocket;
use std::time::Instant;

const ITERATIONS: usize = 1000;

fn main() {
    compio::runtime::Runtime::new().unwrap().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Start echo server with DIRECT IMPLEMENTATION
        let server = compio::runtime::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut rep = RepSocket::new(stream).await.unwrap();
            
            let mut count = 0;
            for i in 0..ITERATIONS {
                match rep.recv().await {
                    Ok(Some(msg)) => {
                        if rep.send(msg).await.is_err() {
                            eprintln!("Server send failed at {}", i);
                            break;
                        }
                        count += 1;
                    }
                    Ok(None) => {
                        eprintln!("Server got None at {}", i);
                        break;
                    }
                    Err(e) => {
                        eprintln!("Server recv error at {}: {}", i, e);
                        break;
                    }
                }
            }
            println!("Server processed {} messages", count);
        });

        // Give server time to start
        compio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Connect client
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut socket = ReqSocket::new(stream).await.unwrap();
        let payload = Bytes::from(vec![0u8; 64]);

        println!("Starting {} round-trips (64B payload)...", ITERATIONS);

        let start = Instant::now();
        let mut success_count = 0;
        for i in 0..ITERATIONS {
            if socket.send(vec![payload.clone()]).await.is_err() {
                println!("Send failed at iteration {}", i);
                break;
            }
            match socket.recv().await {
                Ok(Some(_)) => {
                    success_count += 1;
                },
                Ok(None) => {
                    println!("Connection closed at iteration {}", i);
                    break;
                }
                Err(e) => {
                    println!("Recv error at iteration {}: {}", i, e);
                    break;
                }
            }
        }
        let elapsed = start.elapsed();

        server.await;

        println!("Client completed {} messages", success_count);
        let per_msg = elapsed.as_micros() as f64 / success_count as f64;
        println!("\nResults:");
        println!("  Total time: {:?}", elapsed);
        println!("  Per message: {:.2} µs", per_msg);
        println!("  Throughput: {:.0} msgs/sec", 1_000_000.0 / per_msg);
    });
}
