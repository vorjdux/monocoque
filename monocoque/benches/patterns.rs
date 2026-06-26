//! Pattern-specific benchmarks: PUB/SUB fanout, load balancing, etc.
//!
//! Compares monocoque vs rust-zmq (zmq crate) for different messaging patterns.
//! Measures: Pattern-specific performance characteristics.
//!
//! Tests the PUBLIC API from `monocoque::zmq` (user-facing ergonomics)

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use monocoque::zmq::{PubSocket, SocketOptions, SubSocket};
use std::sync::mpsc;
use std::thread;
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

    for &num_subs in FANOUT_SUBSCRIBERS {
        group.throughput(Throughput::Elements((MESSAGE_COUNT * num_subs) as u64));
        group.bench_with_input(
            BenchmarkId::new("subscribers", num_subs),
            &num_subs,
            |b, &num_subs| {
                b.iter(|| {
                    let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);
                    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

                    // PUB runs on its own OS thread with a dedicated compio runtime.
                    // Sharing a runtime between PUB and SUB causes the compio event
                    // loop to stall for ~30 s after accept_subscriber() completes
                    // because a pending io_uring handshake timer blocks all timer
                    // processing until the 30 s handshake_timeout fires.
                    let payload_pub = payload.clone();
                    let pub_handle = thread::spawn(move || {
                        let rt = compio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
                            let addr = pub_socket.local_addr().unwrap();
                            addr_tx.send(addr).unwrap();

                            for _ in 0..num_subs {
                                pub_socket.accept_subscriber().await.unwrap();
                            }

                            // Let subscription frames propagate to the worker threads.
                            compio::time::sleep(Duration::from_millis(50)).await;

                            for _ in 0..MESSAGE_COUNT {
                                pub_socket.send(vec![payload_pub.clone()]).await.ok();
                            }

                            // Keep socket alive while worker threads flush to TCP.
                            compio::time::sleep(Duration::from_millis(200)).await;
                        });
                    });

                    let server_addr = addr_rx.recv().unwrap();

                    let mut sub_handles = Vec::new();
                    for _ in 0..num_subs {
                        let sub_handle = thread::spawn(move || {
                            let rt = compio::runtime::Runtime::new().unwrap();
                            rt.block_on(async move {
                                let stream =
                                    compio::net::TcpStream::connect(server_addr).await.unwrap();
                                let mut sub = SubSocket::from_tcp_with_options(
                                    stream,
                                    SocketOptions::default().with_buffer_sizes(16384, 16384),
                                )
                                .await
                                .unwrap();
                                sub.subscribe(b"").await.unwrap();

                                let mut count = 0;
                                while count < MESSAGE_COUNT {
                                    match sub.recv().await {
                                        Ok(Some(_)) => count += 1,
                                        Ok(None) | Err(_) => break,
                                    }
                                }
                                count
                            })
                        });
                        sub_handles.push(sub_handle);
                    }

                    pub_handle.join().unwrap();
                    for handle in sub_handles {
                        handle.join().unwrap();
                    }
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

    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));
    group.bench_function("filter_10_percent", |b| {
        b.iter(|| {
            let payload = Bytes::from(vec![0u8; MESSAGE_SIZE]);
            let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

            // Same rationale as monocoque_pubsub_fanout: separate OS threads to
            // avoid the shared-runtime io_uring timer stall after accept_subscriber.
            let payload_pub = payload.clone();
            let pub_handle = thread::spawn(move || {
                let rt = compio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let mut pub_socket = PubSocket::bind("127.0.0.1:0").await.unwrap();
                    let addr = pub_socket.local_addr().unwrap();
                    addr_tx.send(addr).unwrap();

                    pub_socket.accept_subscriber().await.unwrap();

                    compio::time::sleep(Duration::from_millis(50)).await;

                    for i in 0..MESSAGE_COUNT {
                        let topic = if i % 10 == 0 {
                            Bytes::from_static(b"match.topic")
                        } else {
                            Bytes::from_static(b"other.topic")
                        };
                        pub_socket.send(vec![topic, payload_pub.clone()]).await.ok();
                    }

                    compio::time::sleep(Duration::from_millis(200)).await;
                });
            });

            let server_addr = addr_rx.recv().unwrap();

            let sub_handle = thread::spawn(move || {
                let rt = compio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let stream = compio::net::TcpStream::connect(server_addr).await.unwrap();
                    let mut sub = SubSocket::from_tcp(stream).await.unwrap();
                    sub.subscribe(b"match.").await.unwrap();

                    #[allow(
                        clippy::cast_precision_loss,
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss
                    )]
                    let expected = (MESSAGE_COUNT as f64 * matched_ratio) as usize;
                    let mut count = 0;
                    while count < expected {
                        match sub.recv().await {
                            Ok(Some(_)) => count += 1,
                            Ok(None) | Err(_) => break,
                        }
                    }
                    count
                })
            });

            pub_handle.join().unwrap();
            sub_handle.join().unwrap();
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

                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss
                )]
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
