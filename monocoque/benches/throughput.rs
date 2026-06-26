//! Throughput benchmarks: messages per second (PUSH/PULL one-way)
//!
//! Compares monocoque vs rust-zmq (zmq crate, FFI bindings to libzmq) for raw throughput.
//! Measures how many messages can be delivered per second in a PUSH->PULL pipeline.
//!
//! ## Methodology
//!
//! - Sender and receiver run on separate OS threads, each with their own compio runtime.
//! - Timer starts on the PULL side just before the first recv.
//! - Both monocoque and zmq use the same protocol: one send per message, no reply.
//! - Warmup happens outside measurement (connection setup + handshake).
//! - `monocoque_push_pull_coalesced` uses write coalescing (64 KB flush threshold) to
//!   batch multiple sends into a single kernel write, closing the gap with libzmq's
//!   internal IO-thread batching.

use bytes::Bytes;
use compio::net::TcpListener;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024, 4096, 16384];
const BATCH_SIZE: usize = 10_000;

/// Benchmark monocoque PUSH/PULL throughput — eager (one syscall per message).
///
/// PULL binds on a separate OS thread (own compio runtime). PUSH connects and
/// sends in the bench thread. The timer lives on the PULL side: it starts just
/// before the first recv and stops after the last one. That elapsed duration is
/// returned to criterion via `iter_custom`.
fn monocoque_push_pull(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("throughput/monocoque/push_pull");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Bytes::from(vec![0u8; size]);

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (port_tx, port_rx) = mpsc::channel::<u16>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

                    let payload_clone = payload.clone();

                    let pull_thread = thread::spawn(move || {
                        let rt = compio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let port = listener.local_addr().unwrap().port();
                            port_tx.send(port).unwrap();

                            let (stream, _) = listener.accept().await.unwrap();
                            let mut pull = PullSocket::from_tcp_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(16384, 16384),
                            )
                            .await
                            .unwrap();

                            let t0 = std::time::Instant::now();
                            for _ in 0..BATCH_SIZE {
                                pull.recv().await.unwrap();
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                        });
                    });

                    let port = port_rx.recv().unwrap();

                    let push_rt = compio::runtime::Runtime::new().unwrap();
                    push_rt.block_on(async move {
                        let mut push = PushSocket::connect(("127.0.0.1", port)).await.unwrap();
                        for _ in 0..BATCH_SIZE {
                            push.send(vec![payload_clone.clone()]).await.unwrap();
                        }
                    });

                    pull_thread.join().unwrap();
                    total += elapsed_rx.recv().unwrap();
                }

                total
            });
        });
    }

    group.finish();
}

/// Benchmark monocoque PUSH/PULL throughput — with write coalescing enabled.
///
/// Same structure as `monocoque_push_pull` but the PUSH socket batches encoded
/// messages into a 64 KB internal buffer before writing to the kernel.  A
/// manual `flush()` after the loop drains any remainder.  This mirrors the
/// batching that libzmq performs internally via its IO-thread queue.
fn monocoque_push_pull_coalesced(c: &mut Criterion) {
    monocoque::dev_tracing::init_tracing();
    let mut group = c.benchmark_group("throughput/monocoque/push_pull_coalesced");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = Bytes::from(vec![0u8; size]);

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (port_tx, port_rx) = mpsc::channel::<u16>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

                    let payload_clone = payload.clone();

                    let pull_thread = thread::spawn(move || {
                        let rt = compio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let port = listener.local_addr().unwrap().port();
                            port_tx.send(port).unwrap();

                            let (stream, _) = listener.accept().await.unwrap();
                            let mut pull = PullSocket::from_tcp_with_options(
                                stream,
                                SocketOptions::default().with_buffer_sizes(16384, 16384),
                            )
                            .await
                            .unwrap();

                            let t0 = std::time::Instant::now();
                            for _ in 0..BATCH_SIZE {
                                pull.recv().await.unwrap();
                            }
                            elapsed_tx.send(t0.elapsed()).unwrap();
                        });
                    });

                    let port = port_rx.recv().unwrap();

                    let push_rt = compio::runtime::Runtime::new().unwrap();
                    push_rt.block_on(async move {
                        let mut push = PushSocket::connect_with_options(
                            ("127.0.0.1", port),
                            SocketOptions::default()
                                .with_buffer_sizes(16384, 16384)
                                .with_write_coalescing(true),
                        )
                        .await
                        .unwrap();
                        for _ in 0..BATCH_SIZE {
                            push.send(vec![payload_clone.clone()]).await.unwrap();
                        }
                        // Flush remaining bytes that didn't fill the 64 KB threshold.
                        push.flush().await.unwrap();
                    });

                    pull_thread.join().unwrap();
                    total += elapsed_rx.recv().unwrap();
                }

                total
            });
        });
    }

    group.finish();
}

/// Benchmark rust-zmq (libzmq) PUSH/PULL throughput.
///
/// Same structure as `monocoque_push_pull`: PULL binds in a separate thread,
/// PUSH connects in the bench thread. Timer on the PULL side.
fn zmq_push_pull(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput/zmq/push_pull");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        group.throughput(Throughput::Elements(BATCH_SIZE as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let payload = vec![0u8; size];

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    let (endpoint_tx, endpoint_rx) = mpsc::channel::<String>();
                    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

                    let payload_clone = payload.clone();

                    // PULL thread
                    let pull_thread = thread::spawn(move || {
                        let ctx = zmq::Context::new();
                        let pull = ctx.socket(zmq::PULL).unwrap();
                        pull.bind("tcp://127.0.0.1:*").unwrap();
                        let endpoint = pull.get_last_endpoint().unwrap().unwrap();
                        endpoint_tx.send(endpoint).unwrap();

                        let t0 = std::time::Instant::now();
                        for _ in 0..BATCH_SIZE {
                            pull.recv_bytes(0).unwrap();
                        }
                        elapsed_tx.send(t0.elapsed()).unwrap();
                    });

                    let endpoint = endpoint_rx.recv().unwrap();

                    // Small pause so the zmq PULL socket is fully registered before PUSH
                    // connects. libzmq's bind is async internally; 5ms is ample.
                    thread::sleep(Duration::from_millis(5));

                    let ctx = zmq::Context::new();
                    let push = ctx.socket(zmq::PUSH).unwrap();
                    push.connect(&endpoint).unwrap();

                    for _ in 0..BATCH_SIZE {
                        push.send(&payload_clone, 0).unwrap();
                    }

                    pull_thread.join().unwrap();
                    total += elapsed_rx.recv().unwrap();
                }

                total
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
        .sample_size(10);
    targets =
        monocoque_push_pull,
        monocoque_push_pull_coalesced,
        zmq_push_pull
);
criterion_main!(benches);
