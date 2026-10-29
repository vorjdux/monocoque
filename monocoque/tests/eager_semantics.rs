//! Eager delivery: once `send` returns, the message reaches the peer with no
//! further action from the sender, even if the sender's thread then blocks
//! outside the runtime.
//!
//! The sender sends one message and immediately parks its thread on a std
//! channel, so its runtime gets no chance to submit anything after `send`
//! returned. The receiver must still get the exact bytes.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
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
