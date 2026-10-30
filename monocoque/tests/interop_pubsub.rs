//! monocoque PUB against a libzmq SUB.

use bytes::Bytes;
use monocoque::zmq::{Msg, PubSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

#[test]
fn test_pubsub_basic() {
    let (ready_tx, ready_rx) = mpsc::channel::<String>();
    let done = Arc::new(AtomicBool::new(false));
    let publisher = thread::spawn({
        let done = Arc::clone(&done);
        move || {
            monocoque::rt::LocalRuntime::new()
                .unwrap()
                .block_on(async move {
                    let mut publisher = PubSocket::new();
                    ready_tx
                        .send(publisher.bind("tcp://127.0.0.1:0").await.unwrap())
                        .unwrap();
                    // PUB/SUB is a slow joiner: publish until the subscriber has
                    // received, since the first messages race its subscription.
                    while !done.load(Ordering::Relaxed) {
                        publisher
                            .send(Msg::from([
                                Bytes::from_static(b"topic.test"),
                                Bytes::from_static(b"Hello PubSub!"),
                            ]))
                            .await
                            .unwrap();
                        monocoque::rt::sleep(Duration::from_millis(5)).await;
                    }
                });
        }
    });

    let endpoint = ready_rx.recv().unwrap();
    let ctx = zmq::Context::new();
    let sub = ctx.socket(zmq::SUB).unwrap();
    sub.set_rcvtimeo(5000).unwrap();
    sub.set_subscribe(b"topic").unwrap();
    sub.connect(&endpoint).unwrap();
    let msg = sub.recv_multipart(0).unwrap();
    assert_eq!(msg, vec![b"topic.test".to_vec(), b"Hello PubSub!".to_vec()]);
    done.store(true, Ordering::Relaxed);
    publisher.join().unwrap();
}
