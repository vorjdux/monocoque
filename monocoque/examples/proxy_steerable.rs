//! Steerable proxy example: a broker controlled through a PAIR socket.
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
use tracing::info;

const FRONTEND: &str = "inproc://steerable-frontend";
const BACKEND: &str = "inproc://steerable-backend";
const CONTROL: &str = "inproc://steerable-control";

async fn worker(id: u32) -> Result<()> {
    let mut rep = RepSocket::new();
    rep.connect(BACKEND).await?;
    loop {
        let request = rep.recv_owned().await?;
        let text =
            String::from_utf8_lossy(request.frames().next().unwrap_or_default()).into_owned();
        info!("[worker-{id}] processing: {text}");
        rep.send(format!("{text} done by worker-{id}")).await?;
    }
}

async fn broker() -> Result<()> {
    let mut frontend = RouterSocket::new();
    frontend.bind(FRONTEND).await?;
    let mut backend = DealerSocket::new();
    backend.bind(BACKEND).await?;
    let mut control = PairSocket::new();
    control.bind(CONTROL).await?;
    info!("broker up: send PAUSE, RESUME, STATISTICS or TERMINATE on the control socket");
    proxy_steerable(
        &mut frontend,
        &mut backend,
        None::<&mut NoCapture>,
        &mut control,
    )
    .await?;
    info!("broker terminated");
    Ok(())
}

/// One request; `None` if no reply came within the timeout.
async fn request(req: &mut ReqSocket, text: String) -> Result<Option<String>> {
    req.send(text).await?;
    match req.recv_owned().await {
        Ok(reply) => Ok(Some(
            String::from_utf8_lossy(reply.frames().next().unwrap_or_default()).into_owned(),
        )),
        Err(MonocoqueError::TimedOut) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn async_main() -> Result<()> {
    rt::spawn_detached(async {
        if let Err(e) = broker().await {
            tracing::error!("broker: {e}");
        }
    });
    rt::sleep(Duration::from_millis(10)).await;
    for id in 1..=2 {
        rt::spawn_detached(async move {
            let _ = worker(id).await;
        });
    }

    let mut control = PairSocket::with_options(
        SocketOptions::default().with_recv_timeout(Duration::from_secs(2)),
    );
    control.connect(CONTROL).await?;
    let mut req = ReqSocket::with_options(
        SocketOptions::default()
            .with_recv_timeout(Duration::from_millis(300))
            .with_req_relaxed(true)
            .with_req_correlate(true),
    );
    req.connect(FRONTEND).await?;

    for i in 1..=3 {
        let reply = request(&mut req, format!("request {i}")).await?;
        info!("[client] reply: {reply:?}");
    }

    info!("[controller] PAUSE");
    control.send(ProxyCommand::Pause.as_bytes()).await?;
    rt::sleep(Duration::from_millis(20)).await;
    let reply = request(&mut req, "request while paused".to_owned()).await?;
    info!("[client] reply while paused: {reply:?} (held in the broker's sockets)");

    info!("[controller] RESUME");
    control.send(ProxyCommand::Resume.as_bytes()).await?;
    // The held request is forwarded now. The relaxed REQ already gave up on
    // it, so send a fresh one; correlation discards the stale reply.
    let reply = request(&mut req, "request after resume".to_owned()).await?;
    info!("[client] reply: {reply:?}");

    info!("[controller] STATISTICS");
    control.send(ProxyCommand::Statistics.as_bytes()).await?;
    let stats = control.recv_owned().await?;
    let n: Vec<u64> = stats
        .frames()
        .map(|f| f[..].try_into().map_or(0, u64::from_le_bytes))
        .collect();
    info!(
        "[controller] frontend: {} msgs in, {} msgs out; backend: {} msgs in, {} msgs out",
        n[0], n[2], n[4], n[6]
    );

    info!("[controller] TERMINATE");
    control.send(ProxyCommand::Terminate.as_bytes()).await?;
    rt::sleep(Duration::from_millis(50)).await;
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();
    LocalRuntime::new()?.block_on(async_main())
}
