//! DEALER -> ROUTER throughput probe (msg/s), coalescing on vs off.
//!
//! Before 0.4, DEALER ignored `with_write_coalescing` and issued one kernel
//! write per send, so a batching DEALER caller was stuck at eager throughput.
//! 0.4 wires the coalescing/vectored write path into DEALER (and ROUTER, REQ,
//! REP, PAIR). Run with MONOCOQUE_COALESCE=0 and =1 to see the delta:
//!
//!   MONOCOQUE_COALESCE=0 cargo run --release --features zmq --example dealer_throughput_probe
//!   MONOCOQUE_COALESCE=1 cargo run --release --features zmq --example dealer_throughput_probe
//!
//! The receiver (ROUTER) times from the first message to the last and reports
//! msg/s, so the number reflects the full send + kernel + receive pipeline.

use bytes::Bytes;
use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

const MSG: usize = 64;
const COUNT: usize = 1_000_000;

fn main() {
    let coalesce = std::env::var("MONOCOQUE_COALESCE")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let (port_tx, port_rx) = mpsc::channel::<u16>();
    let (rate_tx, rate_rx) = mpsc::channel::<f64>();

    // Receiver: ROUTER drains COUNT messages, timing first-to-last.
    let receiver = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut router = RouterSocket::from_tcp(stream).await.unwrap();

            let mut buf: Vec<Bytes> = Vec::with_capacity(4);
            // First message starts the clock (connection/warmup excluded).
            router.recv_into(&mut buf).await.unwrap();
            let start = Instant::now();
            let mut got = 1usize;
            while got < COUNT {
                if !router.recv_into(&mut buf).await.unwrap() {
                    break;
                }
                got += 1;
            }
            let elapsed = start.elapsed();
            let rate = (got as f64) / elapsed.as_secs_f64();
            rate_tx.send(rate).unwrap();
        });
    });

    let rt = LocalRuntime::new().unwrap();
    rt.block_on(async move {
        let port = port_rx.recv().unwrap();
        let mut dealer = DealerSocket::connect_with_options(
            &format!("tcp://127.0.0.1:{port}"),
            SocketOptions::default().with_write_coalescing(coalesce),
        )
        .await
        .unwrap();
        let payload = Bytes::from(vec![0x5au8; MSG]);
        for _ in 0..COUNT {
            dealer.send(vec![payload.clone()]).await.unwrap();
        }
        dealer.flush().await.unwrap();
    });

    receiver.join().unwrap();
    let rate = rate_rx.recv().unwrap();
    println!(
        "coalesce={coalesce} frame_bytes={MSG} messages={COUNT} throughput={:.2} M msg/s",
        rate / 1_000_000.0
    );
}
