//! Two-process benchmark peer for monocoque.
//!
//! Implements the same wire protocol used by the cross-implementation comparison
//! suite (libzmq bench peer, zmqrs_bench_peer, rzmq_bench_peer) so this binary
//! can participate in cross-implementation comparison runs.
//!
//! ## Subcommands
//!
//! ### TCP transport
//!
//! ```text
//! push         <bind-addr> <size>                   -- coalesced (max throughput)
//! push-eager   <bind-addr> <size>                   -- one write per message (min latency)
//! pull         <connect-addr> <size> <duration_s> [warmup_s]
//! push-fanout  <bind-addr> <size> <workers>         -- ventilator, round-robin to N PULL
//! push-connect <connect-addr> <size>                -- fan-in worker, PUSH that connects
//! pull-fanin   <bind-addr> <size> <duration_s> <workers>  -- sink, merge N PUSH
//! rep          <bind-addr>
//! req          <connect-addr> <size> <iterations> <warmup>
//! pub          <bind-addr> <size>
//! sub          <connect-addr> <size> <duration_s>
//! ```
//!
//! ### IPC (Unix domain socket) transport
//!
//! ```text
//! push-ipc     <path-or-0> <size>                   -- coalesced
//! push-ipc-eager <path-or-0> <size>                 -- eager
//! pull-ipc     <path> <size> <duration_s> [warmup_s]
//! rep-ipc      <path-or-0>
//! req-ipc      <path> <size> <iterations> <warmup>
//! ```
//!
//! For bind-side commands pass `0` to pick a random port / temp socket path.
//! The bound address is printed on stdout as `PORT <n>` (TCP) or `PATH <p>` (IPC)
//! so the caller can connect.
//!
//! ## Wire protocol (throughput)
//!
//! Bind side (push/push-ipc) prints `PORT <n>` or `PATH <p>` then loops
//! sending until killed. Connect side (pull/pull-ipc) counts for
//! `<duration_s>` seconds then prints:
//!
//! ```text
//! <count> <elapsed_secs> <size> <cpu_secs>
//! ```
//!
//! ## Wire protocol (latency)
//!
//! Bind side (rep/rep-ipc) echoes every received message. Connect side
//! (req/req-ipc) measures round-trip time for `<warmup>` + `<iterations>`
//! messages and prints:
//!
//! ```text
//! <p50_us> <p99_us> <p999_us> <max_us> <iterations> <cpu_secs> <elapsed_secs>
//! ```
//!
//! ## Design notes
//!
//! - `push` uses write coalescing with a flush every 64 messages so the
//!   64 KB threshold is exceeded naturally in the timed window while keeping
//!   batch sizes predictable.
//! - `pull` receives into one reused buffer with `recv_into` / `try_recv_into`,
//!   draining every message decoded from a kernel read before the next read and
//!   allocating nothing per message.
//! - No warmup sleep on the pull side. The runner's `read_bound_port`
//!   synchronization is sufficient. (A sleep would fill the kernel send buffer
//!   and deadlock monocoque's single-threaded runtime on a blocked write.)

use std::time::{Duration, Instant};

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque::{
    SocketOptions,
    zmq::{
        PubSocket, PullFanIn, PullSocket, PushFanOut, PushSocket, RepSocket, ReqSocket, SubSocket,
    },
};

#[cfg(unix)]
use compio::net::{UnixListener, UnixStream};

fn cpu_time_secs() -> f64 {
    let mut usage = libc::rusage {
        ru_utime: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        ru_stime: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        // SAFETY: zeroed rusage is valid for all fields.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: passing a valid pointer to a zeroed rusage struct.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let u = usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1e6;
    let s = usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1e6;
    u + s
}

fn resolve_connect(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_digit()) {
        format!("127.0.0.1:{s}")
    } else if let Some(rest) = s.strip_prefix("tcp://") {
        rest.to_owned()
    } else {
        s.to_owned()
    }
}

fn resolve_bind(s: &str) -> String {
    if s == "0" {
        "127.0.0.1:0".to_owned()
    } else if let Some(rest) = s.strip_prefix("tcp://") {
        rest.to_owned()
    } else {
        s.to_owned()
    }
}

/// Parse an optional trailing warmup-seconds argument, defaulting to zero.
fn optional_warmup(args: &[String], idx: usize) -> Duration {
    args.get(idx)
        .map(|s| Duration::from_secs_f64(s.parse().unwrap()))
        .unwrap_or(Duration::ZERO)
}

/// Receive (and discard) for `warmup` so the timed window measures steady state
/// rather than connection ramp-up. Shared by the TCP and IPC pull paths.
async fn warmup_drain<S>(pull: &mut PullSocket<S>, buf: &mut Vec<Bytes>, warmup: Duration)
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    if warmup.is_zero() {
        return;
    }
    let deadline = Instant::now() + warmup;
    'outer: loop {
        match pull.recv_into(buf).await {
            Ok(true) => loop {
                if Instant::now() >= deadline {
                    break 'outer;
                }
                match pull.try_recv_into(buf) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(_) => break 'outer,
                }
            },
            _ => break,
        }
        if Instant::now() >= deadline {
            break;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rt = compio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        match args.get(1).map(String::as_str) {
            // TCP - throughput
            Some("push") => run_push(&resolve_bind(&args[2]), args[3].parse().unwrap(), true).await,
            Some("push-eager") => {
                run_push(&resolve_bind(&args[2]), args[3].parse().unwrap(), false).await
            }
            Some("pull") => {
                run_pull(
                    &resolve_connect(&args[2]),
                    args[3].parse().unwrap(),
                    Duration::from_secs_f64(args[4].parse().unwrap()),
                    optional_warmup(&args, 5),
                )
                .await;
            }
            // TCP - fan-out / fan-in (pipeline pool)
            Some("push-fanout") => {
                run_push_fanout(
                    &resolve_bind(&args[2]),
                    args[3].parse().unwrap(),
                    args[4].parse().unwrap(),
                )
                .await
            }
            Some("push-connect") => {
                run_push_connect(&resolve_connect(&args[2]), args[3].parse().unwrap()).await
            }
            Some("pull-fanin") => {
                run_pull_fanin(
                    &resolve_bind(&args[2]),
                    args[3].parse().unwrap(),
                    Duration::from_secs_f64(args[4].parse().unwrap()),
                    args[5].parse().unwrap(),
                )
                .await;
            }
            // TCP - latency
            Some("rep") => run_rep(&resolve_bind(&args[2])).await,
            Some("req") => {
                run_req(
                    &resolve_connect(&args[2]),
                    args[3].parse().unwrap(),
                    args[4].parse().unwrap(),
                    args[5].parse().unwrap(),
                )
                .await;
            }
            // TCP - pub/sub
            Some("pub") => run_pub(&resolve_bind(&args[2]), args[3].parse().unwrap()).await,
            Some("sub") => {
                run_sub(
                    &resolve_connect(&args[2]),
                    args[3].parse().unwrap(),
                    Duration::from_secs_f64(args[4].parse().unwrap()),
                )
                .await;
            }
            // IPC - throughput
            #[cfg(unix)]
            Some("push-ipc") => {
                run_push_ipc(&ipc_bind_path(&args[2]), args[3].parse().unwrap(), true).await
            }
            #[cfg(unix)]
            Some("push-ipc-eager") => {
                run_push_ipc(&ipc_bind_path(&args[2]), args[3].parse().unwrap(), false).await
            }
            #[cfg(unix)]
            Some("pull-ipc") => {
                run_pull_ipc(
                    &args[2],
                    args[3].parse().unwrap(),
                    Duration::from_secs_f64(args[4].parse().unwrap()),
                    optional_warmup(&args, 5),
                )
                .await;
            }
            // IPC - latency
            #[cfg(unix)]
            Some("rep-ipc") => run_rep_ipc(&ipc_bind_path(&args[2])).await,
            #[cfg(unix)]
            Some("req-ipc") => {
                run_req_ipc(
                    &args[2],
                    args[3].parse().unwrap(),
                    args[4].parse().unwrap(),
                    args[5].parse().unwrap(),
                )
                .await;
            }
            _ => {
                eprintln!(concat!(
                    "usage: monocoque_bench_peer <subcommand> ...\n",
                    "\n",
                    "TCP subcommands:\n",
                    "  push         <addr> <size>                  (coalesced)\n",
                    "  push-eager   <addr> <size>                  (one write/msg)\n",
                    "  pull         <addr> <size> <duration_s> [warmup_s]\n",
                    "  push-fanout  <addr> <size> <workers>        (ventilator)\n",
                    "  push-connect <addr> <size>                  (fan-in worker)\n",
                    "  pull-fanin   <addr> <size> <duration_s> <workers>  (sink)\n",
                    "  rep          <addr>\n",
                    "  req          <addr> <size> <iterations> <warmup>\n",
                    "  pub          <addr> <size>\n",
                    "  sub          <addr> <size> <duration_s>\n",
                    "\n",
                    "IPC subcommands (Unix only):\n",
                    "  push-ipc         <path|0> <size>            (coalesced)\n",
                    "  push-ipc-eager   <path|0> <size>\n",
                    "  pull-ipc         <path>   <size> <duration_s> [warmup_s]\n",
                    "  rep-ipc          <path|0>\n",
                    "  req-ipc          <path>   <size> <iterations> <warmup>\n",
                    "\n",
                    "Pass 0 for <addr>/<path|0> to bind to a random port/temp socket.\n",
                    "Bind side prints PORT <n> (TCP) or PATH <p> (IPC) on stdout.",
                ));
                std::process::exit(1);
            }
        }
    });
}

// ── TCP helpers ───────────────────────────────────────────────────────────────

async fn bind_and_accept(addr: &str) -> (u16, TcpStream) {
    let listener = TcpListener::bind(addr).await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    println!("PORT {port}");
    let (stream, _) = listener.accept().await.expect("accept");
    (port, stream)
}

// ── TCP push/pull ─────────────────────────────────────────────────────────────

async fn run_push(addr: &str, size: usize, coalesce: bool) {
    let (_, stream) = bind_and_accept(addr).await;
    let mut push = if coalesce {
        PushSocket::from_tcp_with_options(
            stream,
            SocketOptions::default().with_write_coalescing(true),
        )
        .await
        .expect("push handshake")
    } else {
        PushSocket::from_tcp(stream).await.expect("push handshake")
    };
    let payload = Bytes::from(vec![b'x'; size]);
    let mut i = 0u64;
    loop {
        push.send(vec![payload.clone()]).await.unwrap_or(());
        if coalesce {
            i += 1;
            if i.is_multiple_of(64) {
                push.flush().await.unwrap_or(());
            }
        }
    }
}

async fn run_pull(addr: &str, size: usize, duration: Duration, warmup: Duration) {
    let mut pull = PullSocket::connect(addr).await.expect("pull connect");

    // One reused message buffer, so the hot recv loop allocates nothing per message.
    let mut buf: Vec<Bytes> = Vec::with_capacity(4);
    warmup_drain(&mut pull, &mut buf, warmup).await;

    let cpu_before = cpu_time_secs();
    let t0 = Instant::now();
    let deadline = t0 + duration;
    let mut count: u64 = 0;

    'outer: loop {
        match pull.recv_into(&mut buf).await {
            Ok(true) => {
                count += 1;
                // Drain any additional messages decoded from the same read batch.
                loop {
                    if Instant::now() >= deadline {
                        break 'outer;
                    }
                    match pull.try_recv_into(&mut buf) {
                        Ok(true) => count += 1,
                        Ok(false) => break,
                        Err(_) => break 'outer,
                    }
                }
            }
            _ => break,
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    let elapsed = t0.elapsed().as_secs_f64();
    let cpu = cpu_time_secs() - cpu_before;
    println!("{count} {elapsed:.6} {size} {cpu:.6}");
    std::process::exit(0);
}

// ── TCP fan-out / fan-in ────────────────────────────────────────────────────────

/// Ventilator: bind, accept `workers` PULL connections, then round-robin tasks
/// across them until killed. Uses write coalescing with a flush every 64 sends,
/// matching the plain `push` throughput path.
async fn run_push_fanout(addr: &str, size: usize, workers: usize) {
    let listener = TcpListener::bind(addr).await.expect("fanout bind");
    let port = listener.local_addr().unwrap().port();
    println!("PORT {port}");
    let mut fanout = PushFanOut::accept_workers(
        &listener,
        workers,
        SocketOptions::default().with_write_coalescing(true),
    )
    .await
    .expect("fanout accept workers");

    let payload = Bytes::from(vec![b'x'; size]);
    // Flush every 64 sends *per worker*. Round-robin spreads each batch across
    // all workers, so flushing every 64 total would leave each worker only
    // 64/workers messages and turn one logical batch into `workers` tiny writes.
    // Scaling the interval lets every worker accumulate ~64 messages per flush,
    // matching the single-stream `push` path.
    let flush_every = (64 * workers.max(1)) as u64;
    let mut i = 0u64;
    loop {
        fanout.send(vec![payload.clone()]).await.unwrap_or(());
        i += 1;
        if i.is_multiple_of(flush_every) {
            fanout.flush().await.unwrap_or(());
        }
    }
}

/// Fan-in worker: connect a PUSH to the sink and send until killed.
async fn run_push_connect(addr: &str, size: usize) {
    let mut push = PushSocket::connect_with_options(
        addr,
        SocketOptions::default().with_write_coalescing(true),
    )
    .await
    .expect("push-connect connect");
    let payload = Bytes::from(vec![b'x'; size]);
    let mut i = 0u64;
    loop {
        push.send(vec![payload.clone()]).await.unwrap_or(());
        i += 1;
        if i.is_multiple_of(64) {
            push.flush().await.unwrap_or(());
        }
    }
}

/// Sink: bind, accept `workers` PUSH connections, count merged messages for
/// `duration`, then print the throughput line and exit.
async fn run_pull_fanin(addr: &str, size: usize, duration: Duration, workers: usize) {
    let listener = TcpListener::bind(addr).await.expect("fanin bind");
    let port = listener.local_addr().unwrap().port();
    println!("PORT {port}");
    let mut sink = PullFanIn::accept_workers(&listener, workers, SocketOptions::default())
        .await
        .expect("fanin accept workers");

    let cpu_before = cpu_time_secs();
    let t0 = Instant::now();
    let deadline = t0 + duration;
    let mut count: u64 = 0;

    'outer: loop {
        match sink.recv().await {
            Ok(Some(_)) => {
                count += 1;
                loop {
                    if Instant::now() >= deadline {
                        break 'outer;
                    }
                    match sink.try_recv() {
                        Ok(Some(_)) => count += 1,
                        Ok(None) => break,
                        Err(_) => break 'outer,
                    }
                }
            }
            _ => break,
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    let elapsed = t0.elapsed().as_secs_f64();
    let cpu = cpu_time_secs() - cpu_before;
    println!("{count} {elapsed:.6} {size} {cpu:.6}");
    std::process::exit(0);
}

// ── TCP rep/req ───────────────────────────────────────────────────────────────

async fn run_rep(addr: &str) {
    let (_, stream) = bind_and_accept(addr).await;
    let mut rep = RepSocket::from_tcp(stream).await.expect("rep handshake");
    while let Ok(Some(msg)) = rep.recv().await {
        if rep.send(msg).await.is_err() {
            break;
        }
    }
}

async fn run_req(addr: &str, size: usize, iterations: usize, warmup: usize) {
    let mut req = ReqSocket::connect(addr).await.expect("req connect");
    let payload = Bytes::from(vec![b'x'; size]);

    for _ in 0..warmup {
        req.send(vec![payload.clone()]).await.unwrap();
        req.recv().await.unwrap();
    }

    let cpu_before = cpu_time_secs();
    let t0 = Instant::now();
    let mut rtts: Vec<u64> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t = Instant::now();
        req.send(vec![payload.clone()]).await.unwrap();
        req.recv().await.unwrap();
        rtts.push(t.elapsed().as_nanos() as u64);
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let cpu = cpu_time_secs() - cpu_before;
    rtts.sort_unstable();

    let percentile = |sorted: &[u64], p: f64| -> f64 {
        let idx = ((sorted.len() as f64 * p / 100.0) as usize).min(sorted.len() - 1);
        sorted[idx] as f64 / 1000.0
    };

    let p50 = percentile(&rtts, 50.0);
    let p99 = percentile(&rtts, 99.0);
    let p999 = percentile(&rtts, 99.9);
    let max = rtts[iterations - 1] as f64 / 1000.0;
    println!("{p50:.3} {p99:.3} {p999:.3} {max:.3} {iterations} {cpu:.6} {elapsed:.6}");
    std::process::exit(0);
}

// ── TCP pub/sub ───────────────────────────────────────────────────────────────

async fn run_pub(addr: &str, size: usize) {
    let mut pub_ = PubSocket::bind(addr).await.expect("pub bind");
    let port = pub_.local_addr().unwrap().port();
    println!("PORT {port}");
    pub_.accept_subscriber().await.expect("accept subscriber");

    let payload = Bytes::from(vec![b'x'; size]);
    loop {
        pub_.send(vec![payload.clone()]).await.unwrap_or(());
    }
}

async fn run_sub(addr: &str, size: usize, duration: Duration) {
    let mut sub = SubSocket::connect(addr).await.expect("sub connect");
    sub.subscribe(b"").await.expect("subscribe");

    let cpu_before = cpu_time_secs();
    let t0 = Instant::now();
    let deadline = t0 + duration;
    let mut count: u64 = 0;

    while Instant::now() < deadline {
        match sub.recv().await {
            Ok(Some(_)) => count += 1,
            _ => break,
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let cpu = cpu_time_secs() - cpu_before;
    println!("{count} {elapsed:.6} {size} {cpu:.6}");
    std::process::exit(0);
}

// ── IPC helpers ───────────────────────────────────────────────────────────────

#[cfg(unix)]
fn ipc_bind_path(arg: &str) -> String {
    if arg == "0" {
        format!("/tmp/monocoque-bench-{}.sock", std::process::id())
    } else if let Some(rest) = arg.strip_prefix("ipc://") {
        rest.to_owned()
    } else {
        arg.to_owned()
    }
}

#[cfg(unix)]
fn ipc_connect_path(arg: &str) -> String {
    if let Some(rest) = arg.strip_prefix("ipc://") {
        rest.to_owned()
    } else {
        arg.to_owned()
    }
}

#[cfg(unix)]
async fn bind_and_accept_ipc(path: &str) -> UnixStream {
    // Remove stale socket file if present.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).await.expect("ipc bind");
    println!("PATH {path}");
    let (stream, _): (UnixStream, _) = listener.accept().await.expect("ipc accept");
    stream
}

// ── IPC push/pull ─────────────────────────────────────────────────────────────

#[cfg(unix)]
async fn run_push_ipc(path: &str, size: usize, coalesce: bool) {
    let stream = bind_and_accept_ipc(path).await;
    let mut push = if coalesce {
        PushSocket::from_unix_stream_with_options(
            stream,
            SocketOptions::default().with_write_coalescing(true),
        )
        .await
        .expect("push-ipc handshake")
    } else {
        PushSocket::from_unix_stream(stream)
            .await
            .expect("push-ipc handshake")
    };
    let payload = Bytes::from(vec![b'x'; size]);
    let mut i = 0u64;
    loop {
        push.send(vec![payload.clone()]).await.unwrap_or(());
        if coalesce {
            i += 1;
            if i.is_multiple_of(64) {
                push.flush().await.unwrap_or(());
            }
        }
    }
}

#[cfg(unix)]
async fn run_pull_ipc(path: &str, size: usize, duration: Duration, warmup: Duration) {
    let connect_path = ipc_connect_path(path);
    let stream = UnixStream::connect(&connect_path)
        .await
        .expect("pull-ipc connect");
    let mut pull = PullSocket::from_unix_stream(stream)
        .await
        .expect("pull-ipc handshake");

    // One reused message buffer, so the hot recv loop allocates nothing per message.
    let mut buf: Vec<Bytes> = Vec::with_capacity(4);
    warmup_drain(&mut pull, &mut buf, warmup).await;

    let cpu_before = cpu_time_secs();
    let t0 = Instant::now();
    let deadline = t0 + duration;
    let mut count: u64 = 0;

    'outer: loop {
        match pull.recv_into(&mut buf).await {
            Ok(true) => {
                count += 1;
                loop {
                    if Instant::now() >= deadline {
                        break 'outer;
                    }
                    match pull.try_recv_into(&mut buf) {
                        Ok(true) => count += 1,
                        Ok(false) => break,
                        Err(_) => break 'outer,
                    }
                }
            }
            _ => break,
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    let elapsed = t0.elapsed().as_secs_f64();
    let cpu = cpu_time_secs() - cpu_before;
    println!("{count} {elapsed:.6} {size} {cpu:.6}");
    std::process::exit(0);
}

// ── IPC rep/req ───────────────────────────────────────────────────────────────

#[cfg(unix)]
async fn run_rep_ipc(path: &str) {
    let stream = bind_and_accept_ipc(path).await;
    let mut rep = RepSocket::from_unix_stream(stream)
        .await
        .expect("rep-ipc handshake");
    while let Ok(Some(msg)) = rep.recv().await {
        if rep.send(msg).await.is_err() {
            break;
        }
    }
}

#[cfg(unix)]
async fn run_req_ipc(path: &str, size: usize, iterations: usize, warmup: usize) {
    let connect_path = ipc_connect_path(path);
    let stream = UnixStream::connect(&connect_path)
        .await
        .expect("req-ipc connect");
    let mut req = ReqSocket::from_unix_stream(stream)
        .await
        .expect("req-ipc handshake");
    let payload = Bytes::from(vec![b'x'; size]);

    for _ in 0..warmup {
        req.send(vec![payload.clone()]).await.unwrap();
        req.recv().await.unwrap();
    }

    let cpu_before = cpu_time_secs();
    let t0 = Instant::now();
    let mut rtts: Vec<u64> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t = Instant::now();
        req.send(vec![payload.clone()]).await.unwrap();
        req.recv().await.unwrap();
        rtts.push(t.elapsed().as_nanos() as u64);
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let cpu = cpu_time_secs() - cpu_before;
    rtts.sort_unstable();

    let percentile = |sorted: &[u64], p: f64| -> f64 {
        let idx = ((sorted.len() as f64 * p / 100.0) as usize).min(sorted.len() - 1);
        sorted[idx] as f64 / 1000.0
    };

    let p50 = percentile(&rtts, 50.0);
    let p99 = percentile(&rtts, 99.0);
    let p999 = percentile(&rtts, 99.9);
    let max = rtts[iterations - 1] as f64 / 1000.0;
    println!("{p50:.3} {p99:.3} {p999:.3} {max:.3} {iterations} {cpu:.6} {elapsed:.6}");
    std::process::exit(0);
}
