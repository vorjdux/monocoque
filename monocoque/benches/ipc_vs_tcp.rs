//! IPC vs TCP comparison benchmarks
//!
//! Compares Unix domain sockets (IPC) vs TCP loopback for latency and throughput.
//!
//! ## Methodology
//!
//! - Server always runs on a separate OS thread with its own runtime.
//! - Latency: `iter_batched`, server exits after warmup+1, one round-trip measured.
//! - Throughput: `iter_custom`, PULL side owns the timer (PUSH/PULL one-way). It
//!   receives one warmup message first, so the timer starts on a live connection.
//! - IPC socket paths include pid + atomic counter to avoid conflicts across iterations.
//! - TCP and IPC run the same code; only the endpoint differs.

#[cfg(unix)]
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
#[cfg(unix)]
use monocoque::zmq::{Msg, PullSocket, PushSocket, RepSocket, ReqSocket, SocketOptions};
#[cfg(unix)]
use std::hint::black_box;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
const MESSAGE_SIZES: &[usize] = &[64, 256, 1024];
#[cfg(unix)]
const MESSAGE_COUNT: usize = 10_000;
#[cfg(unix)]
const WARMUP_ROUNDS: usize = 1_000;

#[cfg(unix)]
static IPC_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique IPC socket path for each call.
#[cfg(unix)]
fn ipc_path(label: &str) -> String {
    let id = IPC_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "/tmp/monocoque_bench_{}_{}_{}.sock",
        std::process::id(),
        label,
        id
    )
}

/// The transport under test.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum Transport {
    Tcp,
    Ipc,
}

#[cfg(unix)]
impl Transport {
    /// An endpoint to bind, plus the socket file to remove afterwards (IPC).
    fn bind_endpoint(self, label: &str) -> (String, Option<String>) {
        match self {
            Self::Tcp => ("tcp://127.0.0.1:0".to_owned(), None),
            Self::Ipc => {
                let path = ipc_path(label);
                let _ = std::fs::remove_file(&path);
                (format!("ipc://{path}"), Some(path))
            }
        }
    }
}

/// Spawn a REP server that echoes `WARMUP_ROUNDS + 1` requests, returning its
/// thread and the endpoint to connect to.
#[cfg(unix)]
fn spawn_rep(transport: Transport) -> (thread::JoinHandle<()>, String) {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let server_thread = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let (endpoint, path) = transport.bind_endpoint("lat");
            let mut rep =
                RepSocket::with_options(SocketOptions::default().with_buffer_sizes(4096, 4096));
            ep_tx.send(rep.bind(&endpoint).await.unwrap()).unwrap();

            for _ in 0..=WARMUP_ROUNDS {
                let Ok(msg) = rep.recv_owned().await else {
                    break;
                };
                if rep.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = rep.close().await;
            if let Some(path) = path {
                let _ = std::fs::remove_file(&path);
            }
        });
    });
    let endpoint = ep_rx.recv().unwrap();
    (server_thread, endpoint)
}

/// REQ/REP latency over `transport`: the server runs on its own thread; the
/// client connects, warms up, then one round-trip is measured.
#[cfg(unix)]
fn monocoque_latency(c: &mut Criterion, name: &str, transport: Transport) {
    let mut group = c.benchmark_group(format!("ipc_vs_tcp/monocoque-{BENCH_BACKEND}/{name}"));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(100);

    // Single runtime reused across iterations.
    let rt = monocoque::rt::LocalRuntime::new().unwrap();

    for &size in MESSAGE_SIZES {
        let payload = Msg::from(vec![0u8; size]);

        group.bench_with_input(
            BenchmarkId::new("round_trip", format!("{size}B")),
            &size,
            |b, _| {
                b.iter_batched(
                    || {
                        let (server_thread, endpoint) = spawn_rep(transport);

                        let req = rt.block_on(async {
                            let mut req = ReqSocket::with_options(
                                SocketOptions::default().with_buffer_sizes(4096, 4096),
                            );
                            req.connect(&endpoint).await.unwrap();

                            for _ in 0..WARMUP_ROUNDS {
                                req.send(payload.clone()).await.unwrap();
                                req.recv().await.unwrap();
                            }

                            req
                        });

                        (req, server_thread)
                    },
                    |(mut req, server_thread)| {
                        let payload = payload.clone();
                        rt.block_on(async move {
                            req.send(black_box(payload)).await.unwrap();
                            let _ = req.recv().await.unwrap();
                            drop(req);
                        });
                        server_thread.join().unwrap();
                    },
                    criterion::BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

#[cfg(unix)]
/// Benchmark monocoque REQ/REP latency over TCP.
fn monocoque_tcp_latency(c: &mut Criterion) {
    monocoque_latency(c, "tcp_latency", Transport::Tcp);
}

#[cfg(unix)]
/// Benchmark monocoque REQ/REP latency over IPC (Unix domain sockets).
fn monocoque_ipc_latency(c: &mut Criterion) {
    monocoque_latency(c, "ipc_latency", Transport::Ipc);
}

/// One timed PUSH/PULL run over `transport`. PULL binds on its own thread and
/// times `MESSAGE_COUNT` receives after a warmup message.
#[cfg(unix)]
fn run_throughput(payload: &Msg, transport: Transport) -> Duration {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

    let pull_thread = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let (endpoint, path) = transport.bind_endpoint("tp");
            let mut pull =
                PullSocket::with_options(SocketOptions::default().with_buffer_sizes(16384, 16384));
            ep_tx.send(pull.bind(&endpoint).await.unwrap()).unwrap();

            pull.recv().await.unwrap();
            let t0 = std::time::Instant::now();
            for _ in 0..MESSAGE_COUNT {
                pull.recv().await.unwrap();
            }
            elapsed_tx.send(t0.elapsed()).unwrap();
            if let Some(path) = path {
                let _ = std::fs::remove_file(&path);
            }
        });
    });

    let endpoint = ep_rx.recv().unwrap();
    let payload = payload.clone();
    let push_rt = monocoque::rt::LocalRuntime::new().unwrap();
    let elapsed = push_rt.block_on(async move {
        let mut push =
            PushSocket::with_options(SocketOptions::default().with_buffer_sizes(16384, 16384));
        push.connect(&endpoint).await.unwrap();
        for _ in 0..=MESSAGE_COUNT {
            push.send(black_box(payload.clone())).await.unwrap();
        }
        // Keep the connection up until the receiver has everything.
        let elapsed = elapsed_rx.recv().unwrap();
        let _ = push.close().await;
        elapsed
    });
    pull_thread.join().unwrap();
    elapsed
}

/// PUSH/PULL throughput over `transport`. PULL binds in a separate thread,
/// PUSH connects in the bench thread. Timer on the PULL side.
#[cfg(unix)]
fn monocoque_throughput(c: &mut Criterion, name: &str, transport: Transport) {
    let mut group = c.benchmark_group(format!("ipc_vs_tcp/monocoque-{BENCH_BACKEND}/{name}"));
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    for &size in MESSAGE_SIZES {
        let payload = Msg::from(vec![0u8; size]);

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter_custom(|iters| (0..iters).map(|_| run_throughput(&payload, transport)).sum());
        });
    }
    group.finish();
}

#[cfg(unix)]
/// Benchmark monocoque PUSH/PULL throughput over TCP.
fn monocoque_tcp_throughput(c: &mut Criterion) {
    monocoque_throughput(c, "tcp_throughput", Transport::Tcp);
}

#[cfg(unix)]
/// Benchmark monocoque PUSH/PULL throughput over IPC (Unix domain sockets).
fn monocoque_ipc_throughput(c: &mut Criterion) {
    monocoque_throughput(c, "ipc_throughput", Transport::Ipc);
}

#[cfg(unix)]
criterion_group!(
    benches,
    monocoque_tcp_latency,
    monocoque_ipc_latency,
    monocoque_tcp_throughput,
    monocoque_ipc_throughput,
);

#[cfg(unix)]
criterion_main!(benches);

#[cfg(not(unix))]
fn main() {
    println!("IPC benchmarks require Unix platform");
}
