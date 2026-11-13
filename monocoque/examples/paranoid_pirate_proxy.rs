//! Paranoid Pirate Pattern with the built-in proxy
//!
//! Same workers and client as `paranoid_pirate_simple`, but the broker is
//! just `proxy()` between a ROUTER and a DEALER.
//!
//! ```text
//! Client (REQ) -> ROUTER (frontend) -> proxy() -> DEALER (backend) <- Workers (DEALER)
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::proxy::{NoCapture, proxy};
use monocoque::zmq::{DealerSocket, MonocoqueError, Msg, ReqSocket, RouterSocket, SocketOptions};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const FRONTEND: &str = "tcp://127.0.0.1:5565";
const BACKEND: &str = "tcp://127.0.0.1:5566";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(2500);

/// Worker with heartbeat logging and crash simulation.
async fn worker(id: u32, crash_after: Option<u32>) -> monocoque::zmq::Result<()> {
    info!("[Worker-{}] Starting", id);

    // Wake up at least once per heartbeat interval even with no requests
    let mut socket =
        DealerSocket::with_options(SocketOptions::default().with_recv_timeout(HEARTBEAT_INTERVAL));
    socket.connect(BACKEND).await?;
    info!("[Worker-{}] READY", id);

    let mut heartbeat_timer = Instant::now();
    let mut count = 0u32;

    loop {
        if heartbeat_timer.elapsed() >= HEARTBEAT_INTERVAL {
            info!("[Worker-{}] HEARTBEAT", id);
            heartbeat_timer = Instant::now();
        }

        if let Some(crash_at) = crash_after
            && count >= crash_at
        {
            error!("[Worker-{}] CRASH!", id);
            return Ok(());
        }

        // Requests arrive as [client identity, empty delimiter, body]
        let msg = match socket.recv_owned().await {
            Ok(msg) => msg,
            Err(MonocoqueError::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        count += 1;
        info!(
            "[Worker-{}] Request #{}: {}",
            id,
            count,
            String::from_utf8_lossy(msg.last().unwrap_or_default())
        );

        rt::sleep(Duration::from_millis(100)).await;

        // Reply with the same envelope so the proxy can route it back
        let mut reply: Msg = (0..msg.len() - 1)
            .filter_map(|i| msg.frame_bytes(i))
            .collect();
        reply.push(format!("Worker-{id} processed request #{count}"));
        socket.send(reply).await?;
    }
}

/// Client that resends on a fresh socket when a request gets no reply.
async fn client(id: u32, requests: u32) -> monocoque::zmq::Result<()> {
    let options = SocketOptions::default().with_recv_timeout(REQUEST_TIMEOUT);
    let mut socket = ReqSocket::with_options(options.clone());
    socket.connect(FRONTEND).await?;

    for i in 1..=requests {
        let req = format!("Request-{i} from Client-{id}");
        loop {
            info!("[Client-{}] Sending: {}", id, req);
            socket.send(req.clone()).await?;
            match socket.recv().await {
                Ok(reply) => {
                    info!(
                        "[Client-{}] Reply: {}",
                        id,
                        String::from_utf8_lossy(reply.first().unwrap_or_default())
                    );
                    break;
                }
                Err(MonocoqueError::TimedOut) => {
                    warn!("[Client-{}] No reply, retrying", id);
                    socket = ReqSocket::with_options(options.clone());
                    socket.connect(FRONTEND).await?;
                }
                Err(e) => return Err(e),
            }
        }
        rt::sleep(Duration::from_millis(800)).await;
    }

    info!("[Client-{}] Done", id);
    Ok(())
}

async fn broker() -> monocoque::zmq::Result<()> {
    let mut frontend = RouterSocket::new();
    frontend.bind(FRONTEND).await?;
    let mut backend = DealerSocket::new();
    backend.bind(BACKEND).await?;
    info!("Broker: proxy between {FRONTEND} and {BACKEND}");
    proxy(&mut frontend, &mut backend, None::<&mut NoCapture>).await
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    rt::spawn_detached(async {
        if let Err(e) = broker().await {
            error!("Broker: {}", e);
        }
    });
    rt::spawn_detached(async {
        let _ = worker(1, None).await;
    });
    rt::spawn_detached(async {
        let _ = worker(2, Some(3)).await;
    });

    rt::sleep(Duration::from_millis(500)).await;
    let c = rt::spawn(async { client(1, 6).await });

    rt::sleep(Duration::from_secs(4)).await;
    info!("Spawning recovery worker");
    rt::spawn_detached(async {
        let _ = worker(3, None).await;
    });

    rt::join(c).await?;
    info!("Demo complete");
    Ok(())
}
