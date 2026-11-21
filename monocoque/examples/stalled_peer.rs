//! A peer that stops reading, and how a socket keeps its other peers going.
//!
//! A send normally returns once the bytes are in the kernel. A peer that
//! stops reading (a hung process, a debugger, a full disk behind it) fills
//! its buffers, its writes stop completing, and a sender that waited for them
//! would stall every other peer along with it.
//!
//! `with_write_stall(Some(window))` bounds that wait. A peer that takes
//! almost nothing for a whole window is marked stalled: sends stop waiting on
//! it until it reads again, and the monitor reports `PeerStalled`. What is
//! queued for it stays queued, up to the send HWM. The default window is
//! 50 ms; `None` waits for every write, bounded only by `send_timeout`.
//!
//! Use the event to log or drop a peer that has gone quiet. Lengthen the
//! window if slow but healthy peers are being reported.
//!
//! Here a ROUTER sends to two DEALERs in turn. One reads; the other says
//! hello and then blocks its thread without closing its connection.
//!
//! Run with: `cargo run --example stalled_peer --features zmq`

use monocoque::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, Msg, RouterSocket, SocketEvent, SocketOptions};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

// The errors cross thread boundaries, so they have to be `Send`.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WINDOW: Duration = Duration::from_millis(100);
/// Large enough that a few hundred fill the kernel's buffers for one peer.
const BODY: usize = 64 * 1024;
/// Rounds to try before deciding the stall was not noticed.
const MAX_ROUNDS: usize = 2_000;
/// Rounds sent after the stall was noticed, to time them.
const ROUNDS_AFTER: usize = 100;
const DONE: &[u8] = b"done";

fn main() -> Result<()> {
    let (endpoint_tx, endpoint_rx) = mpsc::channel::<String>();
    let router = thread::spawn(move || route(&endpoint_tx));

    // The router reports the endpoint it bound. If it failed before that,
    // its end of the channel is gone and its join says why.
    let Ok(endpoint) = endpoint_rx.recv() else {
        return router.join().map_err(|_| "the router thread panicked")?;
    };

    // Dropping `release` at the end of `main` lets the stalled peer go.
    let (release, parked) = mpsc::channel::<()>();
    let stalled_endpoint = endpoint.clone();
    let stalled = thread::spawn(move || stall(&stalled_endpoint, &parked));
    let reader = thread::spawn(move || read(&endpoint));

    router.join().map_err(|_| "the router thread panicked")??;
    let got = reader.join().map_err(|_| "the reader thread panicked")??;
    println!("[reader]  received {got} messages, none held up");

    drop(release);
    stalled
        .join()
        .map_err(|_| "the stalled thread panicked")??;
    Ok(())
}

/// The ROUTER: send to both peers in turn until one is reported stalled, then
/// keep going to show that sends no longer wait on it.
fn route(endpoint_tx: &mpsc::Sender<String>) -> Result<()> {
    LocalRuntime::new()?.block_on(async {
        let mut router = RouterSocket::with_options(
            SocketOptions::default()
                .with_write_stall(Some(WINDOW))
                // Without this, `close` would wait for the messages still
                // queued for the peer that will never read them.
                .with_linger(Some(Duration::ZERO)),
        );
        let events = router.monitor();
        // Port 0: the OS picks a free port and `bind` reports it.
        endpoint_tx.send(router.bind("tcp://127.0.0.1:0").await?)?;

        // Both peers say hello first, so both are routable.
        for _ in 0..2 {
            router.recv().await?;
        }

        let body = Bytes::from(vec![7u8; BODY]);
        let to = |peer: &'static [u8]| Msg::from([Bytes::from_static(peer), body.clone()]);

        let started = Instant::now();
        let mut rounds = 0;
        let stalled = loop {
            if rounds == MAX_ROUNDS {
                return Err("the stalled peer was never reported".into());
            }
            router.send(to(b"stalled")).await?;
            router.send(to(b"reader")).await?;
            rounds += 1;
            let reported = events
                .try_iter()
                .find(|event| matches!(event, SocketEvent::PeerStalled(_)));
            if let Some(event) = reported {
                break event;
            }
        };
        println!(
            "[router]  {stalled} after {rounds} rounds and {:.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );

        // The peer is marked now, so a send to it queues and returns.
        let started = Instant::now();
        for _ in 0..ROUNDS_AFTER {
            router.send(to(b"stalled")).await?;
            router.send(to(b"reader")).await?;
        }
        println!(
            "[router]  {ROUNDS_AFTER} more rounds took {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );

        router
            .send(Msg::from([
                Bytes::from_static(b"reader"),
                Bytes::from_static(DONE),
            ]))
            .await?;
        router.close().await?;
        Ok(())
    })
}

/// A peer that says hello and then stops reading: its thread blocks with the
/// connection open, so nothing takes bytes off its socket.
fn stall(endpoint: &str, parked: &mpsc::Receiver<()>) -> Result<()> {
    LocalRuntime::new()?.block_on(async {
        let mut dealer = DealerSocket::with_options(
            SocketOptions::default().with_routing_id(Bytes::from_static(b"stalled")),
        );
        dealer.connect(endpoint).await?;
        dealer.send("hello").await?;
        println!("[stalled] connected, and not reading from here on");
        // Blocks the thread, runtime and all, until `main` drops the sender.
        let _ = parked.recv();
        Ok(())
    })
}

/// A peer that keeps reading. Returns how many messages it got.
fn read(endpoint: &str) -> Result<usize> {
    LocalRuntime::new()?.block_on(async {
        let mut dealer = DealerSocket::with_options(
            SocketOptions::default()
                .with_routing_id(Bytes::from_static(b"reader"))
                .with_recv_timeout(Duration::from_secs(10)),
        );
        dealer.connect(endpoint).await?;
        dealer.send("hello").await?;
        let mut got = 0;
        loop {
            // The message is only counted, so it stays borrowed.
            let msg = dealer.recv().await?;
            if msg.first() == Some(DONE) {
                break;
            }
            got += 1;
        }
        dealer.close().await?;
        Ok(got)
    })
}
