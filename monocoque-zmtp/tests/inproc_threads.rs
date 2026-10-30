//! inproc between sockets on different threads, where a sender keeps finding
//! the channel at its HWM and the receiver makes room from another thread.
//! A wakeup lost in either direction stalls the run, which the receive
//! timeout turns into a failure.

use monocoque_core::msg::Msg;
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::LocalRuntime;
use monocoque_zmtp::sock::kinds::*;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const COUNT: usize = 200_000;

fn opts() -> SocketOptions {
    SocketOptions::default()
        .with_send_hwm(16)
        .with_recv_hwm(16)
        .with_recv_timeout(Duration::from_secs(10))
        .with_send_timeout(Duration::from_secs(10))
}

#[test]
fn push_to_pull_on_another_thread_never_stalls() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let pull = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut pull = PullSocket::with_options(opts());
            pull.bind("inproc://threads-push-pull").await.unwrap();
            ready_tx.send(()).unwrap();
            for i in 0..COUNT {
                let m = pull
                    .recv_owned()
                    .await
                    .unwrap_or_else(|e| panic!("stalled at message {i}: {e}"));
                assert_eq!(m.first(), Some(&(i as u64).to_be_bytes()[..]));
            }
        });
    });
    ready_rx.recv().unwrap();
    LocalRuntime::new().unwrap().block_on(async move {
        let mut push = PushSocket::with_options(opts());
        push.connect("inproc://threads-push-pull").await.unwrap();
        for i in 0..COUNT {
            push.send(Msg::from((i as u64).to_be_bytes().to_vec()))
                .await
                .unwrap();
        }
    });
    pull.join().unwrap();
}

/// Both directions at once: each thread sends on one channel while it
/// receives on the other, so both runtimes are woken from across threads.
#[test]
fn traffic_both_ways_across_threads_never_stalls() {
    async fn side(bind: &str, connect: &str, ready: mpsc::Sender<()>, go: mpsc::Receiver<()>) {
        let mut pull = PullSocket::with_options(opts());
        pull.bind(bind).await.unwrap();
        ready.send(()).unwrap();
        go.recv().unwrap();
        let mut push = PushSocket::with_options(opts());
        push.connect(connect).await.unwrap();
        let msg = Msg::from(vec![7u8; 32]);
        let send = async {
            for _ in 0..COUNT {
                push.send(msg.clone()).await.unwrap();
            }
        };
        let recv = async {
            for i in 0..COUNT {
                pull.recv()
                    .await
                    .unwrap_or_else(|e| panic!("stalled at message {i}: {e}"));
            }
        };
        futures::join!(send, recv);
    }

    let (a_ready, a_ready_rx) = mpsc::channel();
    let (b_ready, b_ready_rx) = mpsc::channel();
    let (a_go_tx, a_go) = mpsc::channel();
    let (b_go_tx, b_go) = mpsc::channel();
    let a = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(side(
            "inproc://threads-a",
            "inproc://threads-b",
            a_ready,
            a_go,
        ));
    });
    let b = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(side(
            "inproc://threads-b",
            "inproc://threads-a",
            b_ready,
            b_go,
        ));
    });
    // Both are bound before either connects.
    a_ready_rx.recv().unwrap();
    b_ready_rx.recv().unwrap();
    a_go_tx.send(()).unwrap();
    b_go_tx.send(()).unwrap();
    a.join().unwrap();
    b.join().unwrap();
}

/// `flush` waits for an inproc peer to take what was queued. With the peer on
/// another thread nothing on the sender's side signals that, so the link has
/// to wake the flush itself; a missing wakeup parks the sender for good.
#[test]
fn flush_completes_while_a_peer_on_another_thread_drains() {
    for coalesce in [false, true] {
        let name = format!("inproc://threads-flush-{coalesce}");
        let (ready_tx, ready_rx) = mpsc::channel();
        let bind = name.clone();
        let pull = thread::spawn(move || {
            LocalRuntime::new().unwrap().block_on(async move {
                let mut pull = PullSocket::with_options(opts());
                pull.bind(&bind).await.unwrap();
                ready_tx.send(()).unwrap();
                for i in 0..20_000 {
                    pull.recv()
                        .await
                        .unwrap_or_else(|e| panic!("coalesce={coalesce}: stalled at {i}: {e}"));
                }
            });
        });
        ready_rx.recv().unwrap();
        LocalRuntime::new().unwrap().block_on(async move {
            let mut push = PushSocket::with_options(opts().with_write_coalescing(coalesce));
            push.connect(&name).await.unwrap();
            let msg = Msg::from(vec![1u8; 64]);
            for i in 1..=20_000u64 {
                push.send(msg.clone()).await.unwrap();
                if i % 64 == 0 {
                    push.flush().await.unwrap();
                }
            }
            push.close().await.unwrap();
        });
        pull.join().unwrap();
    }
}

/// The same on one thread: the receiver only runs while the flush waits, and
/// each take must wake the flush to look again.
#[test]
fn flush_completes_while_a_peer_on_the_same_thread_drains() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut pull = PullSocket::with_options(opts());
        pull.bind("inproc://threads-flush-local").await.unwrap();
        let mut push = PushSocket::with_options(opts());
        push.connect("inproc://threads-flush-local").await.unwrap();
        let msg = Msg::from(vec![1u8; 64]);
        let send = async {
            for i in 1..=20_000u64 {
                push.send(msg.clone()).await.unwrap();
                if i % 64 == 0 {
                    push.flush().await.unwrap();
                }
            }
        };
        let recv = async {
            for i in 0..20_000 {
                pull.recv()
                    .await
                    .unwrap_or_else(|e| panic!("stalled at {i}: {e}"));
            }
        };
        futures::join!(send, recv);
    });
}
