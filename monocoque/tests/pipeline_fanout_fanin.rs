//! Fan-out and fan-in pipeline tests.
//!
//! These cover the two pool topologies that a single-connection PUSH/PULL pair
//! cannot express on its own:
//!
//! - Fan-out: one `PushFanOut` ventilator spreads tasks across N PULL workers.
//! - Fan-in: N PUSH workers feed one `PullFanIn` sink.
//!
//! Each socket runs in its own thread with its own runtime, matching the
//! pattern used by the other multi-peer tests. Addresses are handed back over a
//! std channel so workers only connect once the bound port is known.

use bytes::Bytes;
use monocoque::SocketOptions;
use monocoque::rt::TcpListener;
use monocoque::zmq::{PullFanIn, PullSocket, PushFanOut, PushSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const WORKERS: usize = 4;
const PER_WORKER: usize = 50;
const TOTAL: usize = WORKERS * PER_WORKER;

/// One ventilator hands `TOTAL` tasks to `WORKERS` PULL workers. Round-robin
/// delivery means each worker should end up with exactly `PER_WORKER` of them,
/// and together they should see every task with none lost or duplicated.
#[test]
fn test_fanout_round_robin_reaches_every_worker() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    // Ventilator: bind, announce the port, accept the pool, then send.
    let ventilator = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut vent =
                    PushFanOut::accept_workers(&listener, WORKERS, SocketOptions::default())
                        .await
                        .unwrap();

                for i in 0..TOTAL {
                    vent.send(vec![Bytes::from(format!("task-{i}"))])
                        .await
                        .unwrap();
                }
                vent.flush().await.unwrap();
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    // Each worker pulls exactly its share, then reports how many it received.
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            thread::spawn(move || {
                monocoque::rt::LocalRuntime::new()
                    .unwrap()
                    .block_on(async move {
                        let mut pull = PullSocket::connect(addr).await.unwrap();
                        let mut count = 0usize;
                        for _ in 0..PER_WORKER {
                            let msg = monocoque::rt::timeout(Duration::from_secs(10), pull.recv())
                                .await
                                .expect("worker recv timed out")
                                .expect("io error")
                                .expect("connection closed");
                            assert!(msg[0].starts_with(b"task-"), "unexpected payload: {msg:?}");
                            count += 1;
                        }
                        count
                    })
            })
        })
        .collect();

    let mut total = 0usize;
    for (i, handle) in workers.into_iter().enumerate() {
        let count = handle.join().expect("worker thread panicked");
        assert_eq!(
            count, PER_WORKER,
            "worker {i} received {count}/{PER_WORKER} tasks"
        );
        total += count;
    }
    assert_eq!(total, TOTAL, "fan-out delivered {total}/{TOTAL} tasks");

    ventilator.join().expect("ventilator thread panicked");
}

/// `WORKERS` PUSH workers each send `PER_WORKER` results to one sink. The sink
/// should merge them all and see exactly `TOTAL` messages.
#[test]
fn test_fanin_merges_every_worker() {
    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();
    let (ready_tx, ready_rx) = mpsc::channel::<()>();

    // Sink: bind, announce the port, accept the pool, then drain TOTAL messages.
    let sink = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                let mut sink =
                    PullFanIn::accept_workers(&listener, WORKERS, SocketOptions::default())
                        .await
                        .unwrap();
                // Every worker is connected now; let them start sending.
                ready_tx.send(()).unwrap();

                let mut count = 0usize;
                while count < TOTAL {
                    let recvd = monocoque::rt::timeout(Duration::from_secs(10), sink.recv())
                        .await
                        .unwrap_or_else(|_| panic!("sink recv timed out at {count}/{TOTAL}"));
                    match recvd {
                        Ok(Some(_)) => count += 1,
                        Ok(None) => break,
                        Err(e) => panic!("sink recv error: {e}"),
                    }
                }
                count
            })
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let workers: Vec<_> = (0..WORKERS)
        .map(|w| {
            thread::spawn(move || {
                monocoque::rt::LocalRuntime::new()
                    .unwrap()
                    .block_on(async move {
                        let mut push = PushSocket::connect(addr).await.unwrap();
                        for i in 0..PER_WORKER {
                            push.send(vec![Bytes::from(format!("result-{w}-{i}"))])
                                .await
                                .unwrap();
                        }
                        push.flush().await.unwrap();
                    });
            })
        })
        .collect();

    // Make sure the sink accepted everyone before we wait on the workers.
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    for handle in workers {
        handle.join().expect("worker thread panicked");
    }

    let count = sink.join().expect("sink thread panicked");
    assert_eq!(count, TOTAL, "fan-in merged {count}/{TOTAL} results");
}

/// Write coalescing and its options must reach the per-worker sockets through
/// `PushFanOut`. With coalescing on and the flush threshold set far above the
/// total payload, the only way the buffered bytes leave userspace is the explicit
/// `flush()`: if `flush()` did not drain every worker, the drain below would hang
/// and fail on its timeout. Receiving every message in order confirms the
/// coalesced-send plus per-worker-flush path works end to end.
#[test]
fn test_fanout_coalescing_send_then_flush_delivers_all() {
    const MSGS: usize = 20;

    let (addr_tx, addr_rx) = mpsc::channel::<std::net::SocketAddr>();

    let ventilator = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();

                // Coalescing on, threshold at 1 MiB so the tiny payloads never reach
                // it on their own: nothing is sent until the explicit flush.
                let options = SocketOptions::default()
                    .with_write_coalescing(true)
                    .with_write_coalesce_threshold(1 << 20);
                let mut vent = PushFanOut::accept_workers(&listener, 1, options)
                    .await
                    .unwrap();

                for i in 0..MSGS {
                    vent.send(vec![Bytes::from(format!("m{i}"))]).await.unwrap();
                }
                // All MSGS are buffered in the worker's coalesce buffer; one flush
                // sends the whole batch.
                vent.flush().await.unwrap();
            });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let worker = thread::spawn(move || {
        monocoque::rt::LocalRuntime::new()
            .unwrap()
            .block_on(async move {
                let mut pull = PullSocket::connect(addr).await.unwrap();
                let mut count = 0usize;
                for i in 0..MSGS {
                    let msg = monocoque::rt::timeout(Duration::from_secs(10), pull.recv())
                        .await
                        .expect("recv timed out: flush did not deliver the batch")
                        .expect("io error")
                        .expect("connection closed");
                    assert_eq!(msg[0], Bytes::from(format!("m{i}")), "out-of-order frame");
                    count += 1;
                }
                count
            })
    });

    let count = worker.join().expect("worker thread panicked");
    assert_eq!(count, MSGS, "worker received {count}/{MSGS} after flush");

    ventilator.join().expect("ventilator thread panicked");
}
