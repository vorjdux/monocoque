//! Eager delivery: once `send` returns, the message reaches the peer with no
//! further action from the sender, even if the sender's thread then blocks
//! outside the runtime.
//!
//! The sender sends one message and immediately parks its thread on a std
//! channel, so its runtime gets no chance to submit anything after `send`
//! returned. The receiver must still get the exact bytes.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const PAYLOAD: &[u8] = b"eager-invariant-exact-bytes";

#[test]
fn eager_send_reaches_peer_with_no_further_sender_action() {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    let sender = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let endpoint = ep_rx.recv().unwrap();
            let mut push = PushSocket::new();
            push.connect(&endpoint).await.unwrap();
            push.send(PAYLOAD).await.unwrap();
            // Block the thread: no further runtime progress on this side.
            release_rx.recv().unwrap();
        });
    });

    LocalRuntime::new().unwrap().block_on(async move {
        let mut pull = PullSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        ep_tx
            .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
            .unwrap();
        let msg = pull
            .recv()
            .await
            .expect("eager send must deliver with no further sender action");
        assert_eq!(msg, [PAYLOAD], "peer must observe the exact bytes sent");
    });

    release_tx.send(()).unwrap();
    sender.join().unwrap();
}

/// Messages too large for one write to finish still reach a peer that is
/// reading, after the sender blocks its thread or drops its socket without
/// `close()`: the send waits while the writes make progress.
fn large_sends_arrive_after(count: usize, size: usize, drop_socket: bool) {
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    let sender = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let endpoint = ep_rx.recv().unwrap();
            let mut push = PushSocket::new();
            push.connect(&endpoint).await.unwrap();
            let body = monocoque::Bytes::from(vec![9u8; size]);
            for _ in 0..count {
                push.send(body.clone()).await.unwrap();
            }
            if drop_socket {
                drop(push);
            }
            release_rx.recv().unwrap();
        });
    });

    LocalRuntime::new().unwrap().block_on(async move {
        let mut pull = PullSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        ep_tx
            .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
            .unwrap();
        for i in 0..count {
            let msg = pull
                .recv()
                .await
                .unwrap_or_else(|e| panic!("message {i} of {count} x {size} B lost: {e}"));
            assert_eq!(msg.frames().next().unwrap().len(), size);
        }
    });

    release_tx.send(()).unwrap();
    sender.join().unwrap();
}

#[test]
fn large_eager_sends_arrive_while_the_sender_blocks() {
    large_sends_arrive_after(20, 64 * 1024, false);
    large_sends_arrive_after(20, 1024 * 1024, false);
}

#[test]
fn large_eager_sends_arrive_after_the_socket_is_dropped() {
    large_sends_arrive_after(20, 64 * 1024, true);
    large_sends_arrive_after(20, 1024 * 1024, true);
}

/// Accept a raw ZMTP 3.0 NULL peer posing as PULL, read `read` bytes every 10 ms
/// until the sender is gone, and return what arrived after the greeting.
fn read_throttled(l: &std::net::TcpListener, read: usize) -> usize {
    let (mut s, _) = l.accept().unwrap();
    let mut g = [0u8; 64];
    g[0] = 0xff;
    g[9] = 0x7f;
    g[10] = 3;
    g[12..16].copy_from_slice(b"NULL");
    s.write_all(&g).unwrap();
    let mut peer = [0u8; 64];
    s.read_exact(&mut peer).unwrap();
    let mut body = vec![5];
    body.extend_from_slice(b"READY");
    body.push(11);
    body.extend_from_slice(b"Socket-Type");
    body.extend_from_slice(&4u32.to_be_bytes());
    body.extend_from_slice(b"PULL");
    s.write_all(&[0x04, u8::try_from(body.len()).unwrap()])
        .unwrap();
    s.write_all(&body).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut buf = vec![0u8; read];
    let mut total = 0;
    while let Ok(k) = s.read(&mut buf) {
        if k == 0 {
            break;
        }
        total += k;
        thread::sleep(Duration::from_millis(10));
    }
    total
}

/// Send `count` messages to the throttled reader, drop the socket, and check
/// every byte arrived.
fn throttled_sends_arrive(count: usize, size: usize) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("tcp://{}", l.local_addr().unwrap());
    let sender = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            // A window well over the reader's pacing: under heavy outside load
            // its thread can pause past the default 50 ms, which would then
            // count as a reader that really stopped.
            let mut push = PushSocket::with_options(
                SocketOptions::default().with_write_stall(Some(Duration::from_millis(200))),
            );
            push.connect(&endpoint).await.unwrap();
            let body = monocoque::Bytes::from(vec![9u8; size]);
            for _ in 0..count {
                push.send(body.clone()).await.unwrap();
            }
        });
    });
    let total = read_throttled(&l, 64 * 1024);
    sender.join().unwrap();
    // Each message has a 9-byte long-frame header; READY comes on top.
    let frames = count * (size + 9);
    assert!(total >= frames, "{total} of {frames} bytes arrived");
}

/// A slow link, where no single write job finishes inside the stall window
/// while bytes keep moving, is not a peer that stopped reading.
#[test]
fn a_slow_reader_is_not_treated_as_stalled() {
    throttled_sends_arrive(4, 2 * 1024 * 1024);
    throttled_sends_arrive(8, 512 * 1024);
}

/// Without a stall window a send waits however long the peer pauses.
#[test]
fn no_stall_window_waits_out_a_pausing_reader() {
    const COUNT: usize = 10;
    const SIZE: usize = 1024 * 1024;
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let sender = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut push =
                PushSocket::with_options(SocketOptions::default().with_write_stall(None));
            push.connect(&ep_rx.recv().unwrap()).await.unwrap();
            let body = monocoque::Bytes::from(vec![9u8; SIZE]);
            for _ in 0..COUNT {
                push.send(body.clone()).await.unwrap();
            }
        });
    });
    LocalRuntime::new().unwrap().block_on(async move {
        let mut pull = PullSocket::with_options(
            SocketOptions::default()
                .with_recv_hwm(1)
                .with_recv_timeout(Duration::from_secs(5)),
        );
        ep_tx
            .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
            .unwrap();
        for i in 0..COUNT {
            pull.recv()
                .await
                .unwrap_or_else(|e| panic!("message {i} lost: {e}"));
            thread::sleep(Duration::from_millis(80));
        }
    });
    sender.join().unwrap();
}

/// Coalescing must not stall a sender at the send HWM: once the HWM is full,
/// held writes go out even though the byte threshold was not reached.
#[test]
fn coalesced_sends_past_the_hwm_make_progress() {
    const HWM: usize = 100;
    const COUNT: usize = 20 * HWM;
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut push = PushSocket::with_options(
            SocketOptions::default()
                .with_write_coalescing(true)
                .with_send_hwm(HWM)
                .with_send_timeout(Duration::from_secs(5)),
        );
        push.connect(&ep).await.unwrap();
        let sender = async {
            for i in 0..COUNT {
                push.send(format!("{i}")).await.unwrap();
            }
            push.flush().await.unwrap();
        };
        let receiver = async {
            for i in 0..COUNT {
                let expected = format!("{i}");
                assert_eq!(pull.recv().await.unwrap(), [expected.as_bytes()]);
            }
        };
        futures::join!(sender, receiver);
    });
}

/// Coalescing with bodies above the vectored-write threshold builds batches
/// longer than one vectored write takes. Every message must stay counted
/// until its last segment is written: otherwise the HWM stops pushing back
/// and `close` returns with most of the batch unwritten, losing it.
#[test]
fn coalesced_large_messages_all_arrive_before_close() {
    const COUNT: usize = 2000;
    const SIZE: usize = 64 * 1024;
    let (ep_tx, ep_rx) = mpsc::channel::<String>();
    let receiver = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pull = PullSocket::with_options(
                SocketOptions::default().with_recv_timeout(Duration::from_secs(10)),
            );
            ep_tx
                .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            for i in 0..COUNT {
                let msg = pull
                    .recv()
                    .await
                    .unwrap_or_else(|e| panic!("message {i} of {COUNT} never arrived: {e}"));
                assert_eq!(msg.first().map(<[u8]>::len), Some(SIZE));
            }
        });
    });
    let endpoint = ep_rx.recv().unwrap();
    LocalRuntime::new().unwrap().block_on(async move {
        let mut push = PushSocket::with_options(
            SocketOptions::default()
                .with_write_coalescing(true)
                .with_vectored_write_threshold(1024),
        );
        push.connect(&endpoint).await.unwrap();
        let payload = monocoque::zmq::Msg::from(vec![3u8; SIZE]);
        for _ in 0..COUNT {
            push.send(payload.clone()).await.unwrap();
        }
        push.close().await.unwrap();
    });
    receiver.join().unwrap();
}
