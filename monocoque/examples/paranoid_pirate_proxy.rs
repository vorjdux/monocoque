//! Reliable request-reply through the built-in proxy.
//!
//! The client and the broker of the Paranoid Pirate pattern, with the broker
//! reduced to `proxy()` between a ROUTER and a DEALER:
//!
//! ```text
//! Client (REQ) -> ROUTER (frontend) -> proxy() -> DEALER (backend) <- Workers (DEALER)
//! ```
//!
//! One worker dies while it holds a request. That request never gets a reply,
//! so the client times out, replaces its socket and sends it again; the retry
//! reaches a live worker. A replacement worker joins while this is going on.
//!
//! Use this when the broker only has to forward. `paranoid_pirate` has the
//! variant whose broker also reads READY and HEARTBEAT messages from workers.
//!
//! Run with: `cargo run --example paranoid_pirate_proxy --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::proxy::{NoCapture, proxy};
use monocoque::zmq::{
    DealerSocket, MonocoqueError, Msg, ReqSocket, Result, RouterSocket, SocketOptions,
};
use std::time::Duration;

const REQUESTS: u32 = 6;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
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
    println!("Broker: clients on {frontend_endpoint}, workers on {backend_endpoint}");

    spawn_worker(1, backend_endpoint.clone());
    // This one dies on its second request, without replying to it.
    let doomed = rt::spawn(worker(2, backend_endpoint.clone(), Some(2)));

    // The backend DEALER balances requests over the workers connected at the
    // time, so wait for both before any request can arrive.
    while backend.peer_count() < 2 {
        rt::sleep(Duration::from_millis(5)).await;
    }

    rt::spawn_detached(async move {
        // `proxy` forwards until one of its sockets fails.
        if let Err(e) = proxy(&mut frontend, &mut backend, None::<&mut NoCapture>).await {
            eprintln!("Broker stopped: {e}");
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

/// Run a worker for the rest of the program, reporting why it stopped if it
/// ever does.
fn spawn_worker(id: u32, backend: String) {
    rt::spawn_detached(async move {
        if let Err(e) = worker(id, backend, None).await {
            eprintln!("[Worker-{id}] stopped: {e}");
        }
    });
}

/// Serve requests. With `die_on` set, the worker dies when it receives that
/// request, counting its own from 1, and the request is lost with it.
async fn worker(id: u32, backend: String, die_on: Option<u32>) -> Result<()> {
    let mut socket = DealerSocket::new();
    socket.connect(&backend).await?;
    println!("[Worker-{id}] Connected");

    let mut count = 0u32;
    loop {
        // A request arrives as [client identity, empty delimiter, body]. The
        // routing frames go back with the reply, so the message is taken by
        // value.
        let request = socket.recv_owned().await?;
        count += 1;
        let body = String::from_utf8_lossy(request.last().unwrap_or_default()).into_owned();

        if die_on == Some(count) {
            println!("[Worker-{id}] Got \"{body}\" and died before replying");
            // Returning drops the socket, which closes the connection.
            return Ok(());
        }

        println!("[Worker-{id}] Working on \"{body}\"");
        rt::sleep(WORK_TIME).await;

        // Same envelope, new body: the proxy routes it back to the client.
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
