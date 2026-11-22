//! `inproc://` between two threads.
//!
//! An inproc endpoint is a name inside the process, not a file or a port. It
//! connects sockets on the same runtime or, as here, on different threads
//! that each run their own `LocalRuntime`. Messages are handed over without
//! serialization or a system call, and each direction is bounded by the
//! HWM like any other transport, so a slow receiver holds the sender back
//! instead of growing a queue.
//!
//! The one rule: an inproc endpoint has to be bound before anything connects
//! to it. TCP redials until the listener appears; an inproc connect to a name
//! nobody has bound fails at once. So the binding thread tells the connecting
//! thread when it is ready, here over a standard channel.
//!
//! Use inproc to pass work between the threads of one program with the same
//! socket patterns that would later let the two halves run as two programs.
//!
//! Run with: `cargo run --example inproc_across_threads --features zmq`

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{RepSocket, ReqSocket};
use std::sync::mpsc;
use std::thread;

// The error crosses a thread boundary, so it has to be `Send`.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const ENDPOINT: &str = "inproc://across-threads";
const REQUESTS: usize = 3;

fn main() -> Result<()> {
    let (bound_tx, bound_rx) = mpsc::channel();
    let server = thread::spawn(move || serve(&bound_tx));

    // Block until the server thread has bound the endpoint. If it failed
    // before that, its end of the channel is gone and the join says why.
    if bound_rx.recv().is_ok() {
        LocalRuntime::new()?.block_on(ask())?;
    }
    server.join().map_err(|_| "the server thread panicked")?
}

/// The binding side, on its own thread and runtime.
fn serve(bound_tx: &mpsc::Sender<()>) -> Result<()> {
    LocalRuntime::new()?.block_on(async {
        let mut rep = RepSocket::new();
        rep.bind(ENDPOINT).await?;
        println!("[server thread] bound {ENDPOINT}");
        bound_tx.send(())?;

        for _ in 0..REQUESTS {
            // The request is only read, so it stays borrowed from the socket
            // until the reply is built.
            let reply = {
                let request = rep.recv().await?;
                let text = String::from_utf8_lossy(request.first().unwrap_or_default());
                println!("[server thread] got \"{text}\"");
                format!("reply to {text}")
            };
            rep.send(reply).await?;
        }
        rep.close().await?;
        Ok(())
    })
}

/// The connecting side, on the main thread.
async fn ask() -> Result<()> {
    let mut req = ReqSocket::new();
    req.connect(ENDPOINT).await?;

    for i in 1..=REQUESTS {
        req.send(format!("request {i}")).await?;
        let reply = req.recv().await?;
        println!(
            "[main thread] got \"{}\"",
            String::from_utf8_lossy(reply.first().unwrap_or_default())
        );
    }
    req.close().await?;
    Ok(())
}
