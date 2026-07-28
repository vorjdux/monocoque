//! C7 footprint baseline: per-connection idle memory cost (manual measurement).
//!
//! This is a MEASUREMENT HARNESS, not a gating assertion. It is `#[ignore]`d so
//! it never runs in the normal test sweep and can never flap CI on RSS noise.
//! Run it explicitly to observe the per-idle-connection memory cost on this
//! machine and backend:
//!
//! ```sh
//! cargo test --features zmq --test idle_wakeups -- --ignored --nocapture
//! # override the connection count:
//! MONOCOQUE_IDLE_CONNS=10000 cargo test --features zmq --test idle_wakeups \
//!     -- --ignored --nocapture
//! ```
//!
//! It stands up `N` idle DEALER<->ROUTER connections (no traffic after the ZMTP
//! handshake), holds every socket alive, and reports the resident-set-size (RSS)
//! delta attributed to those connections. The number is a measured observation
//! of THIS run, printed for the record; nothing is asserted against a hardcoded
//! target, so no fabricated baseline is committed.
//!
//! A precise, portable "RSS at 10k idle connections" gate is not feasible as a
//! committed assertion: RSS is allocator-, kernel-, and backend-dependent and
//! moves with unrelated process state. Hence this on-demand harness instead of a
//! tripwire.

use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;

/// Read this process's resident set size in bytes from `/proc/self/statm`.
///
/// Returns `None` on non-Linux or if the file cannot be read/parsed, so the
/// harness degrades to "measurement unavailable" rather than failing.
fn read_rss_bytes() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    // Fields are in pages: size, resident, shared, text, lib, data, dt.
    let resident_pages: usize = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page_size = 4096usize; // Linux default; good enough for a rough per-conn figure.
    Some(resident_pages * page_size)
}

fn options() -> SocketOptions {
    SocketOptions::default().with_buffer_sizes(4096, 4096)
}

#[test]
#[ignore = "manual C7 footprint measurement harness; run with --ignored --nocapture, not a gate"]
fn per_connection_idle_footprint() {
    let n: usize = std::env::var("MONOCOQUE_IDLE_CONNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);

    let rss_before = read_rss_bytes();

    let (port_tx, port_rx) = mpsc::channel::<u16>();
    let (server_ready_tx, server_ready_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    // Server: accept N connections and hold every ROUTER socket alive, idle.
    let server = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();

            let mut held = Vec::with_capacity(n);
            for _ in 0..n {
                let (stream, _) = listener.accept().await.unwrap();
                let router = RouterSocket::from_tcp_with_options(stream, options())
                    .await
                    .unwrap();
                held.push(router);
            }
            server_ready_tx.send(()).unwrap();
            // Keep the sockets alive and idle until released.
            release_rx.recv().unwrap();
            drop(held);
        });
    });

    let port = port_rx.recv().unwrap();

    // Client: open N connections and hold every DEALER socket alive, idle.
    let (client_ready_tx, client_ready_rx) = mpsc::channel::<()>();
    let (client_release_tx, client_release_rx) = mpsc::channel::<()>();
    let client = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut held = Vec::with_capacity(n);
            for _ in 0..n {
                let dealer =
                    DealerSocket::connect_with_options(&format!("127.0.0.1:{port}"), options())
                        .await
                        .unwrap();
                held.push(dealer);
            }
            client_ready_tx.send(()).unwrap();
            client_release_rx.recv().unwrap();
            drop(held);
        });
    });

    server_ready_rx.recv().unwrap();
    client_ready_rx.recv().unwrap();

    let rss_after = read_rss_bytes();

    match (rss_before, rss_after) {
        (Some(before), Some(after)) => {
            let delta = after.saturating_sub(before);
            let per_conn = delta as f64 / n as f64;
            // Each logical connection is held on both ends (DEALER + ROUTER) in
            // this process, so `per_conn` is the cost of one connection's two
            // socket endpoints combined.
            println!(
                "[C7 idle footprint] connections={n} rss_before={before} bytes \
                 rss_after={after} bytes delta={delta} bytes \
                 per_connection(both_ends)={per_conn:.0} bytes"
            );
        }
        _ => {
            println!(
                "[C7 idle footprint] RSS measurement unavailable on this platform; \
                 established and held {n} idle connections successfully"
            );
        }
    }

    // Release both sides and join.
    release_tx.send(()).unwrap();
    client_release_tx.send(()).unwrap();
    server.join().unwrap();
    client.join().unwrap();
}
