//! Read-batch syscall probe (temporary perf harness).
//!
//! Transfers a fixed bulk of data PUSH -> PULL over TCP loopback with a
//! read_buffer_size taken from MONOCOQUE_READ_BUF (bytes). Run under
//! `strace -f -c -e trace=read` to see how read() syscall count falls as the
//! read batch grows. Prints the bytes received so the transfer is verified.

use bytes::Bytes;
use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;

const MSG: usize = 64 * 1024; // 64 KiB payloads (bulk)
const COUNT: usize = 1024; // 64 MiB total

fn main() {
    let read_buf: usize = std::env::var("MONOCOQUE_READ_BUF")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8192);

    let (port_tx, port_rx) = mpsc::channel::<u16>();

    let sender = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let port = port_rx.recv().unwrap();
            let mut push = PushSocket::connect_with_options(
                ("127.0.0.1", port),
                SocketOptions::default().with_write_coalescing(true),
            )
            .await
            .unwrap();
            let payload = Bytes::from(vec![0x5au8; MSG]);
            for _ in 0..COUNT {
                push.send_one(payload.clone()).await.unwrap();
            }
            push.flush().await.unwrap();
        });
    });

    let rt = LocalRuntime::new().unwrap();
    rt.block_on(async move {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        port_tx.send(listener.local_addr().unwrap().port()).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut pull = PullSocket::from_tcp_with_options(
            stream,
            SocketOptions::default().with_read_buffer_size(read_buf),
        )
        .await
        .unwrap();

        let mut buf: Vec<Bytes> = Vec::with_capacity(4);
        let mut total = 0usize;
        let mut got = 0usize;
        while got < COUNT {
            if !pull.recv_into(&mut buf).await.unwrap() {
                break;
            }
            for f in &buf {
                total += f.len();
            }
            got += 1;
            while got < COUNT && pull.try_recv_into(&mut buf).unwrap() {
                for f in &buf {
                    total += f.len();
                }
                got += 1;
            }
        }
        println!("read_buf={read_buf} received_bytes={total} messages={got}");
    });

    sender.join().unwrap();
}
