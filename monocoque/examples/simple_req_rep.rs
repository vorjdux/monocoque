//! Simple REQ/REP test between Monocoque sockets
//!
//! Run this example:
//! ```bash
//! cargo run --example simple_req_rep --features zmq
//! ```

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{RepSocket, ReqSocket, Result};
use tracing::info;

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Monocoque REQ <-> REP Simple Test ===\n");

    LocalRuntime::new()?.block_on(async {
        // Bind on an ephemeral port; bind returns the resolved endpoint.
        let mut rep = RepSocket::new();
        let endpoint = rep.bind("tcp://127.0.0.1:0").await?;
        info!("[REP] Listening on {}", endpoint);

        let server = rt::spawn(async move {
            // First request-reply cycle
            let request = rep.recv_owned().await?;
            info!("[REP] Received request:");
            for (i, frame) in request.frames().enumerate() {
                info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
            }

            info!("[REP] Sending reply");
            rep.send("Reply from REP").await?;
            rep.close().await?;
            info!("[REP] Request-reply cycle complete");
            Ok::<(), monocoque::zmq::MonocoqueError>(())
        });

        info!("[REQ] Connecting to {}", endpoint);
        let mut req = ReqSocket::new();
        req.connect(&endpoint).await?;

        // Send request. It waits for the connection to come up.
        info!("[REQ] Sending request");
        req.send("Request from REQ").await?;

        // Receive reply
        info!("[REQ] Waiting for reply");
        let msg = req.recv().await?;
        info!("[REQ] Received response:");
        for (i, frame) in msg.frames().enumerate() {
            info!("  Frame {}: {:?}", i, String::from_utf8_lossy(frame));
        }
        info!("[REQ] Request-reply cycle complete");

        info!("Waiting for server task to finish...");
        rt::join(server).await?;
        Ok::<(), monocoque::zmq::MonocoqueError>(())
    })?;

    info!("\nSimple test completed successfully!");
    Ok(())
}
