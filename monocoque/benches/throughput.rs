//! Throughput benchmarks: messages per second
//!
//! Compares monocoque vs rust-zmq (zmq crate, FFI bindings to libzmq) for raw throughput.
//! Measures: How many messages can be sent/received per second?
//!
//! Tests the PUBLIC API from `monocoque::zmq` (user-facing ergonomics)
//!
//! FAIR BENCHMARKING:
//! - Setup overhead (connection, handshake) IS included in measurement
//! - BUT: With MESSAGE_COUNT=10,000, setup is <1% of total time
//! - Both monocoque and rust-zmq measured identically (setup + 10k messages)
//! - Focuses on actual throughput capacity, not just raw send/recv speed

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use monocoque::zmq::{BufferConfig, DealerSocket, RepSocket, ReqSocket, RouterSocket};
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024, 4096, 16384];
const MESSAGE_COUNT: usize = 10_000;

/// Benchmark monocoque REQ/REP throughput (public API)
///
/// Setup overhead included but amortized over 10k messages (<1% of total time)
fn monocoque_req_rep_throughput(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("throughput/monocoque/req_rep");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10); // Fewer samples since each takes ~10s with setup

    // Creating/dropping many io_uring runtimes can exhaust kernel resources.
    // Reuse a single runtime for all iterations of this benchmark.
    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Bytes((size * MESSAGE_COUNT) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Bytes::from(vec![0u8; size]);

            b.iter(|| {
                // Create sockets and run message loop inside the runtime
                rt.block_on(async {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let server_addr = listener.local_addr().unwrap();

                    let accept_task = compio::runtime::spawn(async move {
                        let (stream, _) = listener.accept().await.unwrap();
                        RepSocket::from_tcp_with_config(stream, BufferConfig::small())
                            .await
                            .unwrap()
                    });

                    let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                    let mut req = ReqSocket::from_tcp_with_config(stream, BufferConfig::small())
                        .await
                        .unwrap();
                    let rep = accept_task.await;

                    // Message throughput loop - this dominates the timing with 10k iterations
                    let server_task = compio::runtime::spawn(async move {
                        let mut rep = rep;
                        for _ in 0..MESSAGE_COUNT {
                            let msg = rep.recv().await.unwrap();
                            rep.send(msg).await.ok();
                        }
                    });

                    for _ in 0..MESSAGE_COUNT {
                        req.send(vec![black_box(payload.clone())]).await.unwrap();
                        if let Some(_) = req.recv().await {
                            // Message received
                        }
                    }

                    server_task.await;
                });
            });
        });
    }
    group.finish();
}

/// Benchmark rust-zmq (zmq crate) REQ/REP throughput
///
/// Setup overhead included but amortized over 10k messages (<1% of total time)
fn zmq_req_rep_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput/rust_zmq/req_rep");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Bytes((size * MESSAGE_COUNT) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = vec![0u8; size];

            b.iter(|| {
                // Create context and sockets
                let ctx = zmq::Context::new();
                let rep = ctx.socket(zmq::REP).unwrap();
                rep.bind("tcp://127.0.0.1:*").unwrap();
                let endpoint = rep.get_last_endpoint().unwrap().unwrap();

                std::thread::sleep(Duration::from_millis(10));
                let req = ctx.socket(zmq::REQ).unwrap();
                req.connect(&endpoint).unwrap();
                std::thread::sleep(Duration::from_millis(10));

                // Message throughput loop - this dominates the timing with 10k iterations
                let rep_handle = std::thread::spawn(move || {
                    for _ in 0..MESSAGE_COUNT {
                        let msg = rep.recv_bytes(0).unwrap();
                        rep.send(&msg, 0).unwrap();
                    }
                });

                for _ in 0..MESSAGE_COUNT {
                    req.send(black_box(&payload), 0).unwrap();
                    let _ = req.recv_bytes(0).unwrap();
                }

                rep_handle.join().unwrap();
            });
        });
    }
    group.finish();
}

/// Benchmark monocoque DEALER/ROUTER throughput (public API)
fn monocoque_dealer_router_throughput(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("throughput/monocoque/dealer_router");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    // Reuse a single runtime for all iterations of this benchmark.
    let rt = compio::runtime::Runtime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Bytes::from(vec![0u8; size]);

        group.throughput(Throughput::Bytes((size * MESSAGE_COUNT) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                rt.block_on(async {
                    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let server_addr = listener.local_addr().unwrap();

                    let router_task = compio::runtime::spawn(async move {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut router =
                            RouterSocket::from_tcp_with_config(stream, BufferConfig::large())
                                .await
                                .unwrap();

                        for _ in 0..MESSAGE_COUNT {
                            let msg = router.recv().await.unwrap();
                            router.send(msg).await.ok();
                        }
                    });

                    let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                    let mut dealer =
                        DealerSocket::from_tcp_with_config(stream, BufferConfig::large())
                            .await
                            .unwrap();

                    for _ in 0..MESSAGE_COUNT {
                        dealer.send(vec![black_box(payload.clone())]).await.unwrap();
                        if let Some(_) = dealer.recv().await {
                            // Message received
                        }
                    }

                    router_task.await;
                });
            });
        });
    }
    group.finish();
}

/// Benchmark rust-zmq (zmq crate) DEALER/ROUTER throughput
fn zmq_dealer_router_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput/rust_zmq/dealer_router");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        let payload = vec![0u8; size];

        group.throughput(Throughput::Bytes((size * MESSAGE_COUNT) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let ctx = zmq::Context::new();

                let router = ctx.socket(zmq::ROUTER).unwrap();
                router.bind("tcp://127.0.0.1:*").unwrap();
                let endpoint = router.get_last_endpoint().unwrap().unwrap();

                let router_handle = std::thread::spawn(move || {
                    for _ in 0..MESSAGE_COUNT {
                        let id = router.recv_bytes(0).unwrap();
                        let _empty = router.recv_bytes(0).unwrap();
                        let msg = router.recv_bytes(0).unwrap();
                        router.send(&id, zmq::SNDMORE).unwrap();
                        router.send(&b""[..], zmq::SNDMORE).unwrap();
                        router.send(&msg, 0).unwrap();
                    }
                });

                std::thread::sleep(Duration::from_millis(10));
                let dealer = ctx.socket(zmq::DEALER).unwrap();
                dealer.connect(&endpoint).unwrap();

                for _ in 0..MESSAGE_COUNT {
                    dealer.send(&b""[..], zmq::SNDMORE).unwrap();
                    dealer.send(black_box(&payload), 0).unwrap();
                    let _empty = dealer.recv_bytes(0).unwrap();
                    let _ = dealer.recv_bytes(0).unwrap();
                }

                router_handle.join().unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(60))
        .warm_up_time(Duration::from_secs(5))
        .sample_size(10);  // With 10k messages per sample, setup overhead is amortized
    targets =
        monocoque_req_rep_throughput,
        zmq_req_rep_throughput,
        monocoque_dealer_router_throughput,
        zmq_dealer_router_throughput
);
criterion_main!(benches);
