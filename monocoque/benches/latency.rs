//! Latency benchmarks: steady-state round-trip time in microseconds
//!
//! Compares monocoque vs rust-zmq (zmq crate, FFI bindings to libzmq) for latency.
//! Measures: on an already-established connection, how fast is one message
//! round-trip?
//!
//! ## Methodology
//!
//! - Server runs on a separate OS thread and echoes exactly `WARMUP_ROUNDS +
//!   iters` messages, then exits (deterministic, no disconnect detection).
//! - The connection is established once per `iter_custom` call, outside the timed
//!   region, followed by `WARMUP_ROUNDS` un-measured round-trips.
//! - The timed region is `iters` back-to-back REQ/REP round-trips on that same
//!   persistent connection. Socket teardown and thread join happen AFTER the
//!   timer stops, so these numbers reflect steady-state RTT, not connection
//!   setup or teardown. Both monocoque and zmq are measured identically.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

// Identifies which runtime backend this build benchmarks, so compio, tokio, and smol
// results land under distinct criterion ids instead of overwriting each other.
const BENCH_BACKEND: &str = if cfg!(feature = "runtime-tokio") {
    "tokio"
} else if cfg!(feature = "runtime-smol") {
    "smol"
} else {
    "compio"
};
use monocoque::zmq::{Msg, RepSocket, ReqSocket, SocketOptions};
use std::hint::black_box;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const MESSAGE_SIZES: &[usize] = &[64, 256, 1024];
const WARMUP_ROUNDS: usize = 200;

/// Benchmark monocoque REQ/REP steady-state latency.
///
/// Server binds on its own OS thread and echoes exactly `WARMUP_ROUNDS + iters`
/// messages. The bench thread connects and warms up (not measured), then times
/// `iters` round-trips on the persistent connection.
fn monocoque_req_rep_latency(c: &mut Criterion) {
    monocoque_latency(c, "req_rep", || "tcp://127.0.0.1:0".to_string());
}

/// The same steady-state REQ/REP round trip over a Unix domain socket.
#[cfg(unix)]
fn monocoque_req_rep_ipc_latency(c: &mut Criterion) {
    monocoque_latency(c, "req_rep_ipc", || {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("monocoque-lat-{}-{n}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        format!("ipc://{}", path.display())
    });
}

fn monocoque_latency(c: &mut Criterion, name: &str, endpoint: fn() -> String) {
    let mut group = c.benchmark_group(format!("latency/monocoque-{BENCH_BACKEND}/{name}"));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    // Single runtime reused across all iterations to avoid io_uring resource exhaustion.
    let rt = monocoque::rt::LocalRuntime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Msg::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{size}B")),
            &size,
            |b, _| {
                b.iter_custom(|iters| {
                    let total =
                        WARMUP_ROUNDS + usize::try_from(iters).expect("iteration count fits usize");

                    // SETUP (not measured): server echoes `total` messages, then exits.
                    let (ep_tx, ep_rx) = mpsc::channel::<String>();
                    let bind_to = endpoint();

                    let server_thread = thread::spawn(move || {
                        let rt = monocoque::rt::LocalRuntime::new().unwrap();
                        rt.block_on(async move {
                            let mut rep = RepSocket::with_options(
                                SocketOptions::default().with_buffer_sizes(4096, 4096),
                            );
                            ep_tx.send(rep.bind(&bind_to).await.unwrap()).unwrap();
                            for _ in 0..total {
                                let Ok(msg) = rep.recv_owned().await else {
                                    break;
                                };
                                if rep.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            let _ = rep.close().await;
                        });
                    });

                    let endpoint = ep_rx.recv().unwrap();
                    let payload = payload.clone();

                    let elapsed = rt.block_on(async move {
                        let mut req = ReqSocket::with_options(
                            SocketOptions::default().with_buffer_sizes(4096, 4096),
                        );
                        req.connect(&endpoint).await.unwrap();

                        // Warmup: not measured.
                        for _ in 0..WARMUP_ROUNDS {
                            req.send(payload.clone()).await.unwrap();
                            req.recv().await.unwrap();
                        }

                        // MEASURED: steady-state round-trips on the live connection.
                        let t0 = Instant::now();
                        for _ in 0..iters {
                            req.send(black_box(payload.clone())).await.unwrap();
                            let _ = req.recv().await.unwrap();
                        }
                        t0.elapsed()
                    });

                    server_thread.join().unwrap();
                    elapsed
                });
            },
        );
    }

    group.finish();
}

/// Benchmark rust-zmq (zmq crate, FFI to libzmq) REQ/REP steady-state latency.
///
/// Same structure as the monocoque path: connect + warmup outside the timer,
/// then `iters` round-trips on the persistent connection are measured. The
/// server echoes exactly `WARMUP_ROUNDS + iters` messages, then exits.
fn zmq_req_rep_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency/rust_zmq/req_rep");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    for &size in MESSAGE_SIZES {
        let payload = vec![0u8; size];

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{size}B")),
            &size,
            |b, _| {
                b.iter_custom(|iters| {
                    let total =
                        WARMUP_ROUNDS + usize::try_from(iters).expect("iteration count fits usize");

                    // SETUP (not measured): server echoes `total` messages, then exits.
                    let (endpoint_tx, endpoint_rx) = mpsc::channel::<String>();

                    let server_thread = thread::spawn(move || {
                        let ctx = zmq::Context::new();
                        let rep = ctx.socket(zmq::REP).unwrap();
                        rep.bind("tcp://127.0.0.1:*").unwrap();
                        let endpoint = rep.get_last_endpoint().unwrap().unwrap();
                        endpoint_tx.send(endpoint).unwrap();

                        for _ in 0..total {
                            match rep.recv_bytes(0) {
                                Ok(msg) => {
                                    if rep.send(&msg, 0).is_err() {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    });

                    let endpoint = endpoint_rx.recv().unwrap();

                    let ctx = zmq::Context::new();
                    let req = ctx.socket(zmq::REQ).unwrap();
                    req.connect(&endpoint).unwrap();

                    // Warmup: not measured.
                    for _ in 0..WARMUP_ROUNDS {
                        req.send(&payload, 0).unwrap();
                        let _ = req.recv_bytes(0).unwrap();
                    }

                    // MEASURED: steady-state round-trips on the live connection.
                    let t0 = Instant::now();
                    for _ in 0..iters {
                        req.send(black_box(&payload), 0).unwrap();
                        let _ = req.recv_bytes(0).unwrap();
                    }
                    let elapsed = t0.elapsed();

                    server_thread.join().unwrap();
                    elapsed
                });
            },
        );
    }

    group.finish();
}

#[cfg(unix)]
criterion_group!(
    benches,
    monocoque_req_rep_latency,
    monocoque_req_rep_ipc_latency,
    zmq_req_rep_latency
);
#[cfg(not(unix))]
criterion_group!(benches, monocoque_req_rep_latency, zmq_req_rep_latency);
criterion_main!(benches);
