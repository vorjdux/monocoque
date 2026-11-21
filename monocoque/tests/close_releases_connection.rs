//! A closed socket gives its connections back to the kernel: once the socket
//! is closed and its runtime gone, the peer reads end of stream.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, PushSocket, SocketOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Answer a ZMTP 3 NULL handshake as a PULL socket.
fn handshake(s: &mut TcpStream) {
    let mut greeting = [0u8; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3;
    greeting[12..16].copy_from_slice(b"NULL");
    s.write_all(&greeting).unwrap();
    let mut peer = [0u8; 64];
    s.read_exact(&mut peer).unwrap();
    let mut ready = vec![5u8];
    ready.extend_from_slice(b"READY");
    ready.push(11);
    ready.extend_from_slice(b"Socket-Type");
    ready.extend_from_slice(&4u32.to_be_bytes());
    ready.extend_from_slice(b"PULL");
    s.write_all(&[0x04, u8::try_from(ready.len()).unwrap()])
        .unwrap();
    s.write_all(&ready).unwrap();
}

/// A PULL peer that drains everything it is sent; reports whether it saw
/// end of stream before its read timeout ran out.
fn spawn_peer(listener: TcpListener) -> thread::JoinHandle<bool> {
    thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        handshake(&mut s);
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match s.read(&mut buf) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e) => return e.kind() == std::io::ErrorKind::ConnectionReset,
            }
        }
    })
}

fn run(coalesce: bool) -> bool {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ep = format!("tcp://{}", listener.local_addr().unwrap());
    let peer = spawn_peer(listener);
    let (done_tx, done_rx) = mpsc::channel();
    let client = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let mut push =
                PushSocket::with_options(SocketOptions::default().with_write_coalescing(coalesce));
            push.connect(&ep).await.unwrap();
            for _ in 0..1000 {
                push.send(Msg::from(vec![0u8; 256])).await.unwrap();
            }
            push.flush().await.unwrap();
            push.close().await.unwrap();
        });
        drop(rt);
        // Keep the thread alive: a connection held by something that lives as
        // long as the thread would only close when it exits.
        let _ = done_rx.recv_timeout(Duration::from_secs(5));
    });
    let closed = peer.join().unwrap();
    let _ = done_tx.send(());
    client.join().unwrap();
    closed
}

#[test]
fn closing_an_eager_socket_closes_its_connection() {
    assert!(run(false), "the connection stayed open after close");
}

#[test]
fn closing_a_coalescing_socket_closes_its_connection() {
    assert!(run(true), "the connection stayed open after close");
}
