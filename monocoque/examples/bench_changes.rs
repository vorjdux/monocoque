//! Ad-hoc throughput harness for the perf changes on this branch.
//!
//! Not a criterion benchmark - a focused before/after that toggles each change
//! via its public knob so the effect is isolated:
//!   * vectored writes:   `with_vectored_write_threshold(8K)` vs disabled (MAX)
//!   * receive batching:  `recv_batch()` vs `recv()`
//!   * PUB coalescing:    absolute broadcast throughput (always on)
//!
//! Run with:  cargo run --release --example bench_changes
//!
//! Numbers are machine-specific; the harness prints the ratio so the relative
//! effect is what matters.

use bytes::Bytes;
use monocoque::rt::TcpListener;
use monocoque::zmq::{PubSocket, PullSocket, PushSocket, SocketOptions, SubSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// One PUSH→PULL run. Returns messages/second over the timed window.
///
/// `vectored` toggles the vectored-write threshold; `recv_batch` toggles the
/// receive-side drain; `send_batch` > 1 sends in batches of that size.
fn push_pull(
    size: usize,
    count: usize,
    vectored: bool,
    recv_batch: bool,
    send_batch: usize,
) -> f64 {
    let (port_tx, port_rx) = mpsc::channel::<u16>();
    let (elapsed_tx, elapsed_rx) = mpsc::channel::<Duration>();

    let pull = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut pull = PullSocket::from_tcp(stream).await.unwrap();

            // Untimed sync on the first message, so connection + handshake +
            // first-byte latency stay out of the measured window.
            let _ = pull.recv().await.unwrap();

            let mut received = 0usize;
            let start = Instant::now();
            if recv_batch {
                while received < count {
                    match pull.recv_batch().await.unwrap() {
                        Some(batch) => received += batch.len(),
                        None => break,
                    }
                }
            } else {
                while received < count {
                    match pull.recv().await.unwrap() {
                        Some(_) => received += 1,
                        None => break,
                    }
                }
            }
            elapsed_tx.send(start.elapsed()).unwrap();
        });
    });

    let port = port_rx.recv().unwrap();

    let push = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let opts = SocketOptions::default().with_vectored_write_threshold(if vectored {
                8192
            } else {
                usize::MAX
            });
            let mut push = PushSocket::connect_with_options(("127.0.0.1", port), opts)
                .await
                .unwrap();
            let payload = Bytes::from(vec![0xABu8; size]);

            // One warmup message matches the untimed sync recv on the PULL side.
            push.send(vec![payload.clone()]).await.unwrap();

            if send_batch > 1 {
                let mut sent = 0;
                while sent < count {
                    let n = send_batch.min(count - sent);
                    let batch: Vec<Vec<Bytes>> = (0..n).map(|_| vec![payload.clone()]).collect();
                    push.send_batch(batch).await.unwrap();
                    sent += n;
                }
            } else {
                for _ in 0..count {
                    push.send(vec![payload.clone()]).await.unwrap();
                }
            }
        });
    });

    let elapsed = elapsed_rx.recv().unwrap();
    push.join().unwrap();
    pull.join().unwrap();
    count as f64 / elapsed.as_secs_f64()
}

/// Best msg/s over `runs` repetitions (reduces noise on a shared CPU).
fn best(runs: usize, f: impl Fn() -> f64) -> f64 {
    (0..runs).map(|_| f()).fold(0.0, f64::max)
}

/// PUB→SUB delivered-broadcast throughput to a single subscriber (coalescing
/// always on). PUB sockets drop on HWM, so the publisher oversends in a loop
/// until the subscriber has received `target` messages and signals stop.
fn pub_sub(size: usize, target: usize) -> f64 {
    let (addr_tx, addr_rx) = mpsc::channel::<u16>();
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let pub_thread = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut publisher = PubSocket::bind("127.0.0.1:0").await.unwrap();
            addr_tx
                .send(publisher.local_addr().unwrap().port())
                .unwrap();
            publisher.accept_subscriber().await.unwrap();
            ready_rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(100));

            let payload = Bytes::from(vec![0xCDu8; size]);
            // Oversend until the subscriber says it has enough.
            while stop_rx.try_recv().is_err() {
                publisher
                    .send(vec![Bytes::from_static(b"t"), payload.clone()])
                    .await
                    .unwrap();
            }
        });
    });

    let port = addr_rx.recv().unwrap();
    let sub_thread = thread::spawn(move || {
        let rt = monocoque::rt::LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut sub = SubSocket::connect(&format!("tcp://127.0.0.1:{port}"))
                .await
                .unwrap();
            sub.subscribe(b"").await.unwrap();
            ready_tx.send(()).unwrap();

            let _ = sub.recv().await.unwrap();
            let mut received = 1usize;
            let start = Instant::now();
            while received < target {
                match sub.recv().await.unwrap() {
                    Some(_) => received += 1,
                    None => break,
                }
            }
            start.elapsed()
        })
    });

    let elapsed = sub_thread.join().unwrap();
    let _ = stop_tx.send(());
    pub_thread.join().unwrap();
    target as f64 / elapsed.as_secs_f64()
}

fn m(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.2} M", v / 1_000_000.0)
    } else {
        format!("{:.0} K", v / 1_000.0)
    }
}

fn main() {
    let runs = 2;
    // Optional section selector: `bench_changes 1|2|4` runs one section (each is
    // slow on a shared box); no arg runs all three.
    let sel = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let want = |s: &str| sel == "all" || sel == s;

    if want("1") {
        println!("== Fix 1: vectored writes (PUSH/PULL eager, large frames) ==");
        println!(
            "{:>9} | {:>14} | {:>14} | {:>6}",
            "size", "copy", "vectored", "ratio"
        );
        for &size in &[
            16 * 1024usize,
            32 * 1024,
            64 * 1024,
            128 * 1024,
            256 * 1024,
            1024 * 1024,
        ] {
            // ~150 MB moved per run, floored so large sizes still get a sample.
            let count = (150_000_000 / size).max(400);
            let off = best(runs, || push_pull(size, count, false, false, 1));
            let on = best(runs, || push_pull(size, count, true, false, 1));
            println!(
                "{:>8}K | {:>6} msg/s {:>4.2} GB/s | {:>6} msg/s {:>4.2} GB/s | {:>5.2}x",
                size / 1024,
                m(off),
                off * size as f64 / 1e9,
                m(on),
                on * size as f64 / 1e9,
                on / off
            );
        }
    }

    if want("2") {
        println!("\n== Fix 2: recv_batch vs recv (PUSH send_batch 256, 64 B) ==");
        let count = 500_000;
        let recv_one = best(runs, || push_pull(64, count, false, false, 256));
        let recv_many = best(runs, || push_pull(64, count, false, true, 256));
        println!(
            "recv():       {:>8} msg/s\nrecv_batch(): {:>8} msg/s   ({:.2}x)",
            m(recv_one),
            m(recv_many),
            recv_many / recv_one
        );
    }

    if want("4") {
        println!("\n== Fix 4: PUB/SUB broadcast (1 subscriber, coalescing on) ==");
        for &size in &[64usize, 1024] {
            let tput = best(runs, || pub_sub(size, 200_000));
            println!("{:>6}B: {:>8} msg/s", size, m(tput));
        }
    }
}
