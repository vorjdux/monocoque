//! Paranoid Pirate: reliable request-reply with worker heartbeats.
//!
//! ```text
//! Clients (REQ) -> ROUTER (frontend)
//!                       |
//!                    broker: forwards requests and replies,
//!                       |    keeps READY and HEARTBEAT for itself
//!                  DEALER (backend) <- Workers (DEALER)
//! ```
//!
//! What happens when it runs:
//! 1. Two workers connect and each sends READY.
//! 2. The client sends requests through the broker.
//! 3. One worker dies while it holds a request, so that request is never
//!    answered. The client times out, replaces its socket and sends it again.
//! 4. A replacement worker joins.
//! 5. A worker with nothing to do sends a HEARTBEAT every interval.
//!
//! This broker only counts what the workers tell it. A production broker
//! would bind a ROUTER on the backend to address each worker, keep a queue of
//! the ready ones and drop a worker whose heartbeats stop. When the broker
//! only has to forward, use `proxy()`: see `paranoid_pirate_proxy`.
//!
//! Run with: `cargo run --example paranoid_pirate --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::poll::{PollEvents, PollItem, poll};
use monocoque::zmq::{
    DealerSocket, MonocoqueError, Msg, ReqSocket, Result, RouterSocket, SocketOptions,
};
use std::time::Duration;

// What a worker says to the broker, apart from replies.
const READY: &[u8] = b"\x01";
const HEARTBEAT: &[u8] = b"\x02";

const REQUESTS: u32 = 6;
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(1500);
const WORK_TIME: Duration = Duration::from_millis(100);

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // Port 0: the OS picks free ports and `bind` reports them.
    let mut frontend = RouterSocket::new();
    let frontend_endpoint = frontend.bind("tcp://127.0.0.1:0").await?;
    let mut backend = DealerSocket::new();
    let backend_endpoint = backend.bind("tcp://127.0.0.1:0").await?;
    println!("[Broker] Clients on {frontend_endpoint}, workers on {backend_endpoint}");

    spawn_worker(1, backend_endpoint.clone());
    // This one dies on its second request, without replying to it.
    let doomed = rt::spawn(worker(2, backend_endpoint.clone(), Some(2)));

    // The backend DEALER balances requests over the workers connected at the
    // time, so wait for both before any request can arrive.
    while backend.peer_count() < 2 {
        rt::sleep(Duration::from_millis(5)).await;
    }

    rt::spawn_detached(async move {
        if let Err(e) = broker(frontend, backend).await {
            eprintln!("[Broker] Stopped: {e}");
        }
    });

    let requests = rt::spawn(client(frontend_endpoint));

    // Bring in a replacement as soon as the doomed worker is gone.
    rt::join(doomed).await?;
    println!("Starting a replacement worker");
    spawn_worker(3, backend_endpoint);

    rt::join(requests).await?;
    println!("All {REQUESTS} requests were answered.");
    Ok(())
}

/// Forward requests to the workers and replies to the clients.
///
/// This is written by hand instead of calling `proxy()` because the workers
/// also send READY and HEARTBEAT on the backend. `proxy()` forwards every
/// message, so it would route those to the frontend as if they were replies.
/// The broker has to look at each backend message and keep the control ones.
async fn broker(mut frontend: RouterSocket, mut backend: DealerSocket) -> Result<()> {
    let mut heartbeats = 0u32;
    loop {
        // `poll` reports which sockets have a message without taking it, so
        // nothing is lost when both are ready at once.
        let (from_client, from_worker) = {
            let mut items = [
                PollItem::new(&frontend, PollEvents::POLLIN),
                PollItem::new(&backend, PollEvents::POLLIN),
            ];
            poll(&mut items, None).await?;
            (items[0].is_readable(), items[1].is_readable())
        };

        // Forwarded messages are taken by value: they are sent on as they are.
        if from_client {
            let request = frontend.recv_owned().await?;
            backend.send(request).await?;
        }
        if from_worker {
            let msg = backend.recv_owned().await?;
            if msg == [READY] {
                println!(
                    "[Broker] A worker is ready ({} connected)",
                    backend.peer_count()
                );
            } else if msg == [HEARTBEAT] {
                heartbeats += 1;
                println!("[Broker] Heartbeat #{heartbeats} from an idle worker");
            } else {
                // A reply: its first frame is the client's identity, which is
                // what the ROUTER routes on.
                frontend.send(msg).await?;
            }
        }
    }
}

/// Run a worker for the rest of the program, reporting why it stopped if it
/// ever does.
fn spawn_worker(id: u32, backend: String) {
    rt::spawn_detached(async move {
        if let Err(e) = worker(id, backend, None).await {
            eprintln!("[Worker-{id}] Stopped: {e}");
        }
    });
}

/// Announce READY, then serve requests, sending a HEARTBEAT whenever a whole
/// interval passes without one. With `die_on` set, the worker dies when it
/// receives that request, counting its own from 1, and the request is lost
/// with it.
async fn worker(id: u32, backend: String, die_on: Option<u32>) -> Result<()> {
    // The receive timeout is what wakes an idle worker up to send a heartbeat.
    let mut socket =
        DealerSocket::with_options(SocketOptions::default().with_recv_timeout(HEARTBEAT_INTERVAL));
    socket.connect(&backend).await?;
    socket.send(READY).await?;
    println!("[Worker-{id}] Sent READY");

    let mut count = 0u32;
    loop {
        // A request arrives as [client identity, empty delimiter, body]. The
        // routing frames go back with the reply, so the message is taken by
        // value.
        let request = match socket.recv_owned().await {
            Ok(request) => request,
            Err(MonocoqueError::TimedOut) => {
                socket.send(HEARTBEAT).await?;
                continue;
            }
            Err(e) => return Err(e),
        };
        count += 1;
        let body = String::from_utf8_lossy(request.last().unwrap_or_default()).into_owned();

        if die_on == Some(count) {
            println!("[Worker-{id}] Got \"{body}\" and died before replying");
            // Returning drops the socket, which closes the connection.
            return Ok(());
        }

        println!("[Worker-{id}] Working on \"{body}\"");
        rt::sleep(WORK_TIME).await;

        // Same envelope, new body: the broker routes it back to the client.
        let envelope = request.len().saturating_sub(1);
        let mut reply: Msg = (0..envelope)
            .filter_map(|i| request.frame_bytes(i))
            .collect();
        reply.push(format!("\"{body}\" done by worker {id}"));
        socket.send(reply).await?;
    }
}

/// Send each request until it is answered.
async fn client(frontend: String) -> Result<()> {
    let options = SocketOptions::default().with_recv_timeout(REQUEST_TIMEOUT);
    let mut socket = ReqSocket::with_options(options.clone());
    socket.connect(&frontend).await?;

    for i in 1..=REQUESTS {
        let request = format!("request {i}");
        loop {
            println!("[Client] Sending \"{request}\"");
            socket.send(request.clone()).await?;

            // REQ pairs a reply with its request, but nothing promises that a
            // reply comes: a request that died with its worker is never
            // answered. A REQ socket that is still owed a reply cannot send
            // again, so on a timeout it is replaced and the request resent.
            match socket.recv().await {
                Ok(reply) => {
                    println!(
                        "[Client] Reply: {}",
                        String::from_utf8_lossy(reply.first().unwrap_or_default())
                    );
                    break;
                }
                Err(MonocoqueError::TimedOut) => {
                    println!(
                        "[Client] No reply to \"{request}\" in {REQUEST_TIMEOUT:?}, \
                         retrying on a new socket"
                    );
                    socket = ReqSocket::with_options(options.clone());
                    socket.connect(&frontend).await?;
                }
                Err(e) => return Err(e),
            }
        }
    }
    socket.close().await
}
