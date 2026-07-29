//! DEALER write-batching syscall probe (temporary perf harness).
//!
//! Sends a fixed number of small frames DEALER -> ROUTER over TCP loopback with
//! write coalescing toggled by MONOCOQUE_COALESCE (0/1, default 0). Run under
//! `strace -f -c -e trace=write,writev,sendto` to see the DEALER-side write
//! syscall count collapse once coalescing is honored. Before PERF 0.3, DEALER
//! ignored `write_coalescing` and issued one write per send regardless.
//!
//! The ROUTER side only receives, so post-handshake write syscalls are the
//! DEALER's. Prints the frames drained so the transfer is verified.

use bytes::Bytes;
use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;

const MSG: usize = 64; // small frames: coalescing packs many per flush
const COUNT: usize = 20_000;

fn main() {
    let coalesce = std::env::var("MONOCOQUE_COALESCE")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let (port_tx, port_rx) = mpsc::channel::<u16>();

    // Receiver: ROUTER drains COUNT messages and exits.
    let receiver = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut router = RouterSocket::from_tcp(stream).await.unwrap();
            let mut got = 0usize;
            while got < COUNT {
                match router.recv().await.unwrap() {
                    Some(_) => got += 1,
                    None => break,
                }
            }
            got
        })
    });

    let rt = LocalRuntime::new().unwrap();
    let sent = rt.block_on(async move {
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
        // Drain any bytes still buffered by the coalescing path.
        dealer.flush().await.unwrap();
        COUNT
    });

    let drained = receiver.join().unwrap();
    println!("coalesce={coalesce} sent={sent} drained={drained} frame_bytes={MSG}");
}
