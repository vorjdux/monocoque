//! Simple direct performance test: REQ/REP round-trips on one runtime.

use monocoque::zmq::{Msg, RepSocket, ReqSocket};
use std::time::Instant;

const ITERATIONS: usize = 1000;

fn main() {
    monocoque::rt::LocalRuntime::new().unwrap().block_on(async {
        let mut rep = RepSocket::new();
        let endpoint = rep.bind("tcp://127.0.0.1:0").await.unwrap();

        // Start echo server
        let server = monocoque::rt::spawn(async move {
            let mut count = 0;
            for i in 0..ITERATIONS {
                match rep.recv_owned().await {
                    Ok(msg) => {
                        if rep.send(msg).await.is_err() {
                            eprintln!("Server send failed at {i}");
                            break;
                        }
                        count += 1;
                    }
                    Err(e) => {
                        eprintln!("Server recv error at {i}: {e}");
                        break;
                    }
                }
            }
            println!("Server processed {count} messages");
            let _ = rep.close().await;
        });

        // Connect client
        let mut socket = ReqSocket::new();
        socket.connect(&endpoint).await.unwrap();
        let payload = Msg::from(vec![0u8; 64]);

        println!("Starting {ITERATIONS} round-trips (64B payload)...");

        let start = Instant::now();
        let mut success_count = 0;
        for i in 0..ITERATIONS {
            if socket.send(payload.clone()).await.is_err() {
                println!("Send failed at iteration {i}");
                break;
            }
            match socket.recv().await {
                Ok(_) => {
                    success_count += 1;
                }
                Err(e) => {
                    println!("Recv error at iteration {i}: {e}");
                    break;
                }
            }
        }
        let elapsed = start.elapsed();

        monocoque::rt::join(server).await;

        println!("Client completed {success_count} messages");
        let per_msg = elapsed.as_secs_f64() * 1_000_000.0 / f64::from(success_count);
        println!("\nResults:");
        println!("  Total time: {elapsed:?}");
        println!("  Per message: {per_msg:.2} µs");
        println!("  Throughput: {:.0} msgs/sec", 1_000_000.0 / per_msg);
    });
}
