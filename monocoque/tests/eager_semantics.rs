//! Permanent eager-invariant tripwire (charter C1).
//!
//! The invariant: in the default (eager) send mode, once `send(...).await`
//! resolves on the sender, the exact message is already on its way to the peer
//! with NO further sender action - no `flush()`, no additional `send()`, not
//! even dropping the socket. Eager mode makes `flush()` a no-op (write coalescing
//! is opt-in), so a single awaited `send` must be self-sufficient.
//!
//! ## How the test proves "no further sender action"
//!
//! After the single `send().await` resolves, the sender signals `sent` and then
//! parks, waiting for the receiver's `received` signal before it touches the
//! socket again. The socket is neither flushed nor dropped in that window. The
//! receiver reads the message purely on the strength of that one `send`, so if
//! the message only arrived because of a later flush/drop, the receiver would
//! block forever and the test would hang (then fail on the join timeout guard).
//!
//! Runs on the default compio backend and must stay simple and robust.

use bytes::Bytes;
use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const ADDR: &str = "127.0.0.1:0";
const PAYLOAD: &[u8] = b"eager-invariant-exact-bytes";

#[test]
fn eager_send_reaches_peer_with_no_further_sender_action() {
    let (port_tx, port_rx) = mpsc::channel::<u16>();
    // sender -> main: "I have awaited exactly one send and will now do nothing".
    let (sent_tx, sent_rx) = mpsc::channel::<()>();
    // main -> sender: "the peer observed it; you may now tear down".
    let (release_tx, release_rx) = mpsc::channel::<()>();

    let sender = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let port = port_rx.recv().unwrap();
            // Default options => eager mode (write coalescing OFF). No flush path.
            let mut push = PushSocket::connect_with_options(
                ("127.0.0.1", port),
                SocketOptions::default().with_buffer_sizes(16384, 16384),
            )
            .await
            .unwrap();

            // Exactly one awaited send. Nothing else touches the socket until the
            // receiver has observed the message.
            push.send(vec![Bytes::from_static(PAYLOAD)]).await.unwrap();
            sent_tx.send(()).unwrap();

            // Park with the socket alive and idle: no flush, no extra send, no
            // drop. If eager delivery were not self-sufficient, the peer could
            // never receive, and this test would deadlock.
            release_rx.recv().unwrap();
            drop(push);
        });
    });

    let rt = LocalRuntime::new().unwrap();
    rt.block_on(async move {
        let listener = TcpListener::bind(ADDR).await.unwrap();
        port_tx.send(listener.local_addr().unwrap().port()).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut pull = PullSocket::from_tcp_with_options(
            stream,
            SocketOptions::default().with_buffer_sizes(16384, 16384),
        )
        .await
        .unwrap();

        // Wait until the sender has completed its single send and is parked.
        sent_rx.recv().unwrap();

        // The message must be observable now, on the strength of that one send.
        let msg = pull
            .recv()
            .await
            .unwrap()
            .expect("eager send must deliver the message with no further sender action");
        assert_eq!(msg.len(), 1, "expected a single-frame message");
        assert_eq!(
            msg[0].as_ref(),
            PAYLOAD,
            "peer must observe the exact bytes the sender enqueued"
        );

        // Let the sender tear down now that the invariant is proven.
        release_tx.send(()).unwrap();
    });

    // Guard against a silent hang masking a broken invariant: if the sender
    // thread cannot finish promptly, something kept the socket from delivering.
    for _ in 0..50 {
        if sender.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        sender.is_finished(),
        "sender did not finish; eager delivery may not be self-sufficient"
    );
    sender.join().unwrap();
}
