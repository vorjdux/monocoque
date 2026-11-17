//! Steerable proxy: a broker controlled through a PAIR socket.
//!
//! Commands: PAUSE, RESUME, STATISTICS, TERMINATE.
//!
//! ```text
//! Client (REQ) -> ROUTER (frontend) <-> DEALER (backend) -> Workers (REP)
//!                          |
//!                  control socket (PAIR)
//!                          |
//!                     controller
//! ```
//!
//! Use it when a broker has to be paused, measured or shut down cleanly from
//! elsewhere in the program. A plain `proxy` only stops on an error.
//!
//! Everything runs in one process on inproc endpoints:
//! ```bash
//! cargo run --example proxy_steerable --features zmq
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::proxy::{NoCapture, ProxyCommand, proxy_steerable};
use monocoque::zmq::{
    DealerSocket, MonocoqueError, PairSocket, RepSocket, ReqSocket, Result, RouterSocket,
    SocketOptions,
};
use std::time::Duration;

const FRONTEND: &str = "inproc://steerable-frontend";
const BACKEND: &str = "inproc://steerable-backend";
const CONTROL: &str = "inproc://steerable-control";

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // An inproc endpoint has to be bound before anything connects to it, so
    // the broker's sockets are bound here and then handed to its task.
    let mut frontend = RouterSocket::new();
    frontend.bind(FRONTEND).await?;
    let mut backend = DealerSocket::new();
    backend.bind(BACKEND).await?;
    let mut broker_control = PairSocket::new();
    broker_control.bind(CONTROL).await?;

    let broker = rt::spawn(async move {
        // Returns `Ok` once TERMINATE arrives on the control socket.
        proxy_steerable(
            &mut frontend,
            &mut backend,
            None::<&mut NoCapture>,
            &mut broker_control,
        )
        .await
    });

    for id in 1..=2 {
        rt::spawn_detached(async move {
            if let Err(e) = worker(id).await {
                eprintln!("[worker-{id}] stopped: {e}");
            }
        });
    }

    let mut control = PairSocket::with_options(
        SocketOptions::default().with_recv_timeout(Duration::from_secs(2)),
    );
    control.connect(CONTROL).await?;
    // A relaxed REQ may send again without waiting for the reply it is owed,
    // and correlation lets it discard a late reply to an abandoned request.
    let mut req = ReqSocket::with_options(
        SocketOptions::default()
            .with_recv_timeout(Duration::from_millis(300))
            .with_req_relaxed(true)
            .with_req_correlate(true),
    );
    req.connect(FRONTEND).await?;

    for i in 1..=3 {
        let reply = request(&mut req, format!("request {i}")).await?;
        println!("[client] reply: {reply:?}");
    }

    println!("[controller] PAUSE");
    control.send(ProxyCommand::Pause.as_bytes()).await?;
    // Commands are handled in order, so once this reply is back the PAUSE
    // has taken effect.
    statistics(&mut control).await?;
    let reply = request(&mut req, "request while paused".to_owned()).await?;
    println!("[client] reply while paused: {reply:?} (the request waits in the frontend)");

    println!("[controller] RESUME");
    control.send(ProxyCommand::Resume.as_bytes()).await?;
    // The held request is forwarded now. The client already gave up on it,
    // so it sends a fresh one and the stale reply is discarded.
    let reply = request(&mut req, "request after resume".to_owned()).await?;
    println!("[client] reply: {reply:?}");

    println!("[controller] STATISTICS");
    let stats = statistics(&mut control).await?;
    // Messages in, bytes in, messages out, bytes out: frontend, then backend.
    if let [front_in, _, front_out, _, back_in, _, back_out, _] = stats[..] {
        println!(
            "[controller] frontend: {front_in} msgs in, {front_out} msgs out; \
             backend: {back_in} msgs in, {back_out} msgs out"
        );
    } else {
        println!("[controller] unexpected statistics reply: {stats:?}");
    }

    println!("[controller] TERMINATE");
    control.send(ProxyCommand::Terminate.as_bytes()).await?;
    rt::join(broker).await?;
    println!("broker terminated");
    Ok(())
}

async fn worker(id: u32) -> Result<()> {
    let mut rep = RepSocket::new();
    rep.connect(BACKEND).await?;
    loop {
        // The request is only read, so it stays borrowed from the socket
        // until the reply is built.
        let reply = {
            let request = rep.recv().await?;
            let text = String::from_utf8_lossy(request.first().unwrap_or_default());
            println!("[worker-{id}] processing: {text}");
            format!("{text} done by worker-{id}")
        };
        rep.send(reply).await?;
    }
}

/// One request; `None` if no reply came within the timeout.
async fn request(req: &mut ReqSocket, text: String) -> Result<Option<String>> {
    req.send(text).await?;
    match req.recv().await {
        Ok(reply) => Ok(Some(
            String::from_utf8_lossy(reply.first().unwrap_or_default()).into_owned(),
        )),
        Err(MonocoqueError::TimedOut) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Ask the proxy for its counters: eight little-endian 64-bit frames.
async fn statistics(control: &mut PairSocket) -> Result<Vec<u64>> {
    control.send(ProxyCommand::Statistics.as_bytes()).await?;
    let reply = control.recv().await?;
    Ok(reply
        .frames()
        .map(|frame| frame.try_into().map_or(0, u64::from_le_bytes))
        .collect())
}
