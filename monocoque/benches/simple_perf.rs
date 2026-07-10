//! Simple direct performance test - BOTH DIRECT IMPLEMENTATIONS

use bytes::Bytes;
use monocoque::rt::{TcpListener, TcpStream};
use monocoque_zmtp::rep::RepSocket;
use monocoque_zmtp::req::ReqSocket;
use std::time::Instant;

const ITERATIONS: usize = 1000;

fn main() {
    monocoque::rt::LocalRuntime::new().unwrap().block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Start echo server with DIRECT IMPLEMENTATION
        let server = monocoque::rt::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut rep = RepSocket::new(stream).await.unwrap();

            let mut count = 0;
            for i in 0..ITERATIONS {
                match rep.recv().await {
                    Ok(Some(msg)) => {
                        if rep.send(msg).await.is_err() {
                            eprintln!("Server send failed at {i}");
                            break;
                        }
                        count += 1;
                    }
                    Ok(None) => {
                        eprintln!("Server got None at {i}");
                        break;
                    }
                    Err(e) => {
                        eprintln!("Server recv error at {i}: {e}");
                        break;
                    }
                }
            }
            println!("Server processed {count} messages");
        });

        // Give server time to start
        monocoque::rt::sleep(std::time::Duration::from_millis(10)).await;

        // Connect client
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut socket = ReqSocket::new(stream).await.unwrap();
        let payload = Bytes::from(vec![0u8; 64]);

        println!("Starting {ITERATIONS} round-trips (64B payload)...");

        let start = Instant::now();
        let mut success_count = 0;
        for i in 0..ITERATIONS {
            if socket.send(vec![payload.clone()]).await.is_err() {
                println!("Send failed at iteration {i}");
                break;
            }
            match socket.recv().await {
                Ok(Some(_)) => {
                    success_count += 1;
                }
                Ok(None) => {
                    println!("Connection closed at iteration {i}");
                    break;
                }
                Err(e) => {
                    println!("Recv error at iteration {i}: {e}");
                    break;
                }
            }
        }
        let elapsed = start.elapsed();

        let () = server.await;

        println!("Client completed {success_count} messages");
        let per_msg = elapsed.as_secs_f64() * 1_000_000.0 / f64::from(success_count);
        println!("\nResults:");
        println!("  Total time: {elapsed:?}");
        println!("  Per message: {per_msg:.2} µs");
        println!("  Throughput: {:.0} msgs/sec", 1_000_000.0 / per_msg);
    });
}
