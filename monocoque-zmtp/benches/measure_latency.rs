//! Quick latency measurement
//!
//! This measures the actual round-trip latency without Criterion overhead.

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use monocoque_zmtp::{RepSocket, ReqSocket};

const WARMUP: usize = 10;
const ITERATIONS: usize = 1000;

async fn measure_latency() -> Duration {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Start server
    compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut rep = RepSocket::new(stream).await.unwrap();
        for _ in 0..(WARMUP + ITERATIONS) {
            if let Ok(Some(msg)) = rep.recv().await {
                rep.send(msg).await.ok();
            }
        }
    })
    .detach();

    compio::time::sleep(Duration::from_millis(50)).await;

    // Connect client
    let stream = TcpStream::connect(addr).await.unwrap();
    let mut socket = ReqSocket::new(stream).await.unwrap();
    let payload = Bytes::from(vec![0u8; 64]);

    // Warmup
    for _ in 0..WARMUP {
        socket.send(vec![payload.clone()]).await.unwrap();
        socket.recv().await.unwrap();
    }

    // Measure
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        socket.send(vec![payload.clone()]).await.unwrap();
        socket.recv().await.unwrap();
    }
    start.elapsed()
}

fn main() {
    compio::runtime::Runtime::new().unwrap().block_on(async {
        println!("Measuring {} iterations (64B payload)...\n", ITERATIONS);

        let total = measure_latency().await;
        let per_msg = total.as_micros() as f64 / ITERATIONS as f64;
        println!(
            "Total time: {:?}\nPer message: {:.2} µs\nThroughput: {:.0} msg/s",
            total,
            per_msg,
            1_000_000.0 / per_msg
        );
    });
}
