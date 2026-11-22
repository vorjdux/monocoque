//! Quick latency measurement
//!
//! This measures the actual round-trip latency without Criterion overhead.

use monocoque::zmq::{Msg, RepSocket, ReqSocket};
use std::time::{Duration, Instant};

const WARMUP: usize = 10;
const ITERATIONS: usize = 1000;

#[allow(clippy::future_not_send)]
async fn measure_latency() -> Duration {
    let mut rep = RepSocket::new();
    let endpoint = rep.bind("tcp://127.0.0.1:0").await.unwrap();

    // Start server
    let server = monocoque::rt::spawn(async move {
        for _ in 0..(WARMUP + ITERATIONS) {
            let Ok(msg) = rep.recv_owned().await else {
                break;
            };
            rep.send(msg).await.ok();
        }
        let _ = rep.close().await;
    });

    // Connect client
    let mut socket = ReqSocket::new();
    socket.connect(&endpoint).await.unwrap();
    let payload = Msg::from(vec![0u8; 64]);

    // Warmup
    for _ in 0..WARMUP {
        socket.send(payload.clone()).await.unwrap();
        socket.recv().await.unwrap();
    }

    // Measure
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        socket.send(payload.clone()).await.unwrap();
        socket.recv().await.unwrap();
    }
    let elapsed = start.elapsed();
    monocoque::rt::join(server).await;
    elapsed
}

fn main() {
    monocoque::rt::LocalRuntime::new().unwrap().block_on(async {
        println!("Measuring {ITERATIONS} iterations (64B payload)...\n");

        let total = measure_latency().await;
        #[allow(clippy::cast_precision_loss)]
        let per_msg = total.as_secs_f64() * 1_000_000.0 / ITERATIONS as f64;
        println!(
            "Total time: {:?}\nPer message: {:.2} µs\nThroughput: {:.0} msg/s",
            total,
            per_msg,
            1_000_000.0 / per_msg
        );
    });
}
