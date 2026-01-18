//! Pattern-specific benchmarks: PUB/SUB fanout, load balancing, etc.
//!
//! Compares monocoque vs rust-zmq (zmq crate) for different messaging patterns.
//! Measures: Pattern-specific performance characteristics.
//!
//! Tests the PUBLIC API from `monocoque::zmq` (user-facing ergonomics)

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use monocoque::zmq::{BufferConfig, PubSocket, SubSocket};
use std::time::Duration;

// NOTE: Multi-subscriber fanout is currently not benchmarked here because the
// direct-stream PUB/SUB sockets do not yet support stable multi-peer fanout.
const FANOUT_SUBSCRIBERS: &[usize] = &[1];
const MESSAGE_COUNT: usize = 100; // Reduced for reasonable benchmark times (was 10_000)
const MESSAGE_SIZE: usize = 256;

/// Benchmark monocoque PUB/SUB fanout (1 publisher, N subscribers)
fn monocoque_pubsub_fanout(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("patterns/monocoque/pubsub_fanout");

    // Creating/dropping many io_uring runtimes can exhaust kernel resources.
    // Reuse a single runtime for all iterations of this benchmark.
    let rt = compio::runtime::Runtime::new().unwrap();

    for &num_subs in FANOUT_SUBSCRIBERS {
        group.throughput(Throughput::Elements((MESSAGE_COUNT * num_subs) as u64));
        group.bench_with_input(
            BenchmarkId::new("subscribers", num_subs),
            &num_subs,
            |b, &num_subs| {
                b.iter(|| {
                    rt.block_on(async {
                        let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);

                        // Start PUB server
                        let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
                        let server_addr = pub_socket.local_addr().unwrap();

                        // Accept subscriber connections and spawn pub task
                        let pub_task = compio::runtime::spawn(async move {
                            // Accept N subscribers
                            for _ in 0..num_subs {
                                pub_socket.accept_subscriber().await.unwrap();
                            }

                            // Wait for subscriptions
                            compio::time::sleep(Duration::from_millis(50)).await;

                            // Publish messages
                            for _ in 0..MESSAGE_COUNT {
                                pub_socket.send(vec![payload.clone()]).await.ok();
                            }
                        });

                        // Start N subscribers
                        let mut sub_tasks = Vec::new();
                        for _i in 0..num_subs {
                            let server_addr = server_addr;
                            let task = compio::runtime::spawn(async move {
                                let stream =
                                    compio::net::TcpStream::connect(server_addr).await.unwrap();
                                let mut sub =
                                    SubSocket::from_tcp_with_config(stream, BufferConfig::large())
                                        .await
                                        .unwrap();
                                sub.subscribe(b""); // Subscribe to all

                                let mut count = 0;
                                while count < MESSAGE_COUNT {
                                    if sub.recv().await.ok().flatten().is_some() {
                                        count += 1;
                                    }
                                }
                                count
                            });
                            sub_tasks.push(task);
                        }

                        // Wait for pub task
                        pub_task.await;

                        // Wait for all subscribers to complete
                        for task in sub_tasks {
                            let _ = task.await;
                        }
                    });
                });
            },
        );
    }
    group.finish();
}

/// Benchmark rust-zmq (zmq crate) PUB/SUB fanout
fn zmq_pubsub_fanout(c: &mut Criterion) {
    let mut group = c.benchmark_group("patterns/rust_zmq/pubsub_fanout");

    for &num_subs in FANOUT_SUBSCRIBERS {
        group.throughput(Throughput::Elements((MESSAGE_COUNT * num_subs) as u64));
        group.bench_with_input(
            BenchmarkId::new("subscribers", num_subs),
            &num_subs,
            |b, &num_subs| {
                b.iter(|| {
                    let payload = vec![0u8; MESSAGE_SIZE];
                    let ctx = zmq::Context::new();

                    let pub_socket = ctx.socket(zmq::PUB).unwrap();
                    pub_socket.bind("tcp://127.0.0.1:*").unwrap();
                    let endpoint = pub_socket.get_last_endpoint().unwrap().unwrap();

                    let mut sub_handles = Vec::new();
                    for _ in 0..num_subs {
                        let endpoint = endpoint.clone();
                        let handle = std::thread::spawn(move || {
                            let ctx = zmq::Context::new();
                            let sub = ctx.socket(zmq::SUB).unwrap();
                            sub.connect(&endpoint).unwrap();
                            sub.set_subscribe(b"").unwrap();

                            let mut count = 0;
                            while count < MESSAGE_COUNT {
                                if sub.recv_bytes(0).is_ok() {
                                    count += 1;
                                }
                            }
                            count
                        });
                        sub_handles.push(handle);
                    }

                    std::thread::sleep(Duration::from_millis(50));

                    for _ in 0..MESSAGE_COUNT {
                        pub_socket.send(black_box(&payload), 0).unwrap();
                    }

                    for handle in sub_handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

/// Benchmark monocoque topic filtering efficiency
fn monocoque_topic_filtering(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("patterns/monocoque/topic_filtering");

    let matched_ratio = 0.1; // 10% of messages match subscription

    // Reuse a single runtime for all iterations of this benchmark.
    let rt = compio::runtime::Runtime::new().unwrap();

    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));
    group.bench_function("filter_10_percent", |b| {
        b.iter(|| {
            rt.block_on(async {
                let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);

                let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
                let server_addr = pub_socket.local_addr().unwrap();

                let pub_task = compio::runtime::spawn(async move {
                    // Accept subscriber
                    pub_socket.accept_subscriber().await.unwrap();

                    compio::time::sleep(Duration::from_millis(50)).await;

                    // Publish mix of matching and non-matching messages
                    for i in 0..MESSAGE_COUNT {
                        let topic = if i % 10 == 0 {
                            Bytes::from_static(b"match.topic")
                        } else {
                            Bytes::from_static(b"other.topic")
                        };
                        pub_socket.send(vec![topic, payload.clone()]).await.ok();
                    }
                });

                let sub_task = compio::runtime::spawn(async move {
                    let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                    let mut sub = SubSocket::from_tcp(stream).await.unwrap();
                    sub.subscribe(b"match.");

                    let expected = (MESSAGE_COUNT as f64 * matched_ratio) as usize;
                    let mut count = 0;
                    while count < expected {
                        if sub.recv().await.ok().flatten().is_some() {
                            count += 1;
                        }
                    }
                });

                pub_task.await;
                sub_task.await;
            });
        });
    });

    group.finish();
}

/// Benchmark rust-zmq topic filtering efficiency
fn zmq_topic_filtering(c: &mut Criterion) {
    let mut group = c.benchmark_group("patterns/rust_zmq/topic_filtering");

    let matched_ratio = 0.1;

    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));
    group.bench_function("filter_10_percent", |b| {
        b.iter(|| {
            let payload = vec![0u8; MESSAGE_SIZE];
            let ctx = zmq::Context::new();

            let pub_socket = ctx.socket(zmq::PUB).unwrap();
            pub_socket.bind("tcp://127.0.0.1:*").unwrap();
            let endpoint = pub_socket.get_last_endpoint().unwrap().unwrap();

            let sub_handle = std::thread::spawn(move || {
                let ctx = zmq::Context::new();
                let sub = ctx.socket(zmq::SUB).unwrap();
                sub.connect(&endpoint).unwrap();
                sub.set_subscribe(b"match.").unwrap();

                let expected = (MESSAGE_COUNT as f64 * matched_ratio) as usize;
                let mut count = 0;
                while count < expected {
                    if sub.recv_bytes(0).is_ok() {
                        count += 1;
                    }
                }
            });

            std::thread::sleep(Duration::from_millis(50));

            for i in 0..MESSAGE_COUNT {
                let topic: &[u8] = if i % 10 == 0 {
                    b"match.topic"
                } else {
                    b"other.topic"
                };
                pub_socket.send(topic, zmq::SNDMORE).unwrap();
                pub_socket.send(black_box(&payload), 0).unwrap();
            }

            sub_handle.join().unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .sample_size(30);
    targets =
        monocoque_pubsub_fanout,
        zmq_pubsub_fanout,
        monocoque_topic_filtering,
        zmq_topic_filtering
);
criterion_main!(benches);
