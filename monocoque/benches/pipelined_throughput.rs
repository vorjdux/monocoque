//! Monocoque batch send API throughput benchmark
//!
//! This benchmark shows the speedup from monocoque's explicit batching API
//! (`send_buffered()` + `flush()`) compared to naive one-send-per-message.
//! It is NOT a cross-library comparison -- use throughput.rs for that.
//!
//! ## Pattern
//!
//! Process messages in batches to avoid TCP backpressure:
//! 1. DEALER: `send_buffered` batch -> flush
//! 2. ROUTER: receive batch -> `send_buffered` batch -> flush
//! 3. DEALER: receive batch
//! 4. Repeat
//!
//! DEALER and ROUTER run on separate OS threads (each with its own compio
//! runtime) so the measured time reflects real inter-thread communication.

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

// Identifies which runtime backend this build benchmarks, so compio, tokio, and smol
// results land under distinct criterion ids instead of overwriting each other.
const BENCH_BACKEND: &str = if cfg!(feature = "runtime-tokio") {
    "tokio"
} else if cfg!(feature = "runtime-smol") {
    "smol"
} else {
    "compio"
};
use monocoque::rt::TcpListener;
use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024, 4096, 16384];
const TOTAL_MESSAGES: usize = 10_000;
const BATCH_SIZE: usize = 100;

/// Benchmark monocoque DEALER/ROUTER pipelined throughput using the batch API.
///
/// ROUTER binds in a separate OS thread. DEALER connects in the bench thread.
/// Both use `send_buffered + flush` to minimize syscall overhead.
fn monocoque_dealer_router_pipelined(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group(format!("pipelined/monocoque-{BENCH_BACKEND}/dealer_router"));
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.throughput(Throughput::Bytes((size * TOTAL_MESSAGES) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (port_tx, port_rx) = mpsc::channel::<u16>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();
                    let payload_clone = payload.clone();

                    // ROUTER thread: bind, receive + echo in batches, report elapsed.
                    let router_thread = thread::spawn(move || {
                        let rt = monocoque::rt::LocalRuntime::new().unwrap();
                        rt.block_on(async move {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let port = listener.local_addr().unwrap().port();
                            port_tx.send(port).unwrap();

                            let (stream, _) = listener.accept().await.unwrap();
                            let mut router = RouterSocket::from_tcp_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(16384, 16384),
                            )
                            .await
                            .unwrap();

                            let t0 = std::time::Instant::now();
                            for _ in 0..(TOTAL_MESSAGES / BATCH_SIZE) {
                                let mut batch = Vec::with_capacity(BATCH_SIZE);
                                for _ in 0..BATCH_SIZE {
                                    if let Ok(Some(msg)) = router.recv().await {
                                        batch.push(msg);
                                    }
                                }
                                for msg in batch {
                                    router.send_buffered(msg).unwrap();
                                }
                                router.flush().await.unwrap();
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                        });
                    });

                    let port = port_rx.recv().unwrap();

                    // DEALER thread: connect, send + recv in batches.
                    let dealer_rt = monocoque::rt::LocalRuntime::new().unwrap();
                    dealer_rt.block_on(async move {
                        let stream = monocoque::rt::TcpStream::connect(("127.0.0.1", port))
                            .await
                            .unwrap();
                        let mut dealer = DealerSocket::from_tcp_with_options(
                            stream,
                            SocketOptions::default().with_buffer_sizes(16384, 16384),
                        )
                        .await
                        .unwrap();

                        for _ in 0..(TOTAL_MESSAGES / BATCH_SIZE) {
                            for _ in 0..BATCH_SIZE {
                                dealer
                                    .send_buffered(vec![black_box(payload_clone.clone())])
                                    .unwrap();
                            }
                            dealer.flush().await.unwrap();

                            for _ in 0..BATCH_SIZE {
                                if dealer.recv().await.ok().flatten().is_none() {
                                    break;
                                }
                            }
                        }
                    });

                    router_thread.join().unwrap();
                    total += elapsed_rx.recv().unwrap();
                }

                total
            });
        });
    }
    group.finish();
}

criterion_group!(benches, monocoque_dealer_router_pipelined);
criterion_main!(benches);
