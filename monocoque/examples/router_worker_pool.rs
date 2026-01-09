/// ROUTER Worker Pool Example
///
/// This example demonstrates a ROUTER socket acting as a load balancer
/// distributing work across multiple DEALER workers.
///
/// Architecture:
/// - ROUTER server listens on port 5555
/// - Multiple DEALER clients connect and request work
/// - ROUTER distributes tasks in round-robin fashion
use bytes::Bytes;
use compio::net::TcpListener;
use monocoque::zmq::RouterSocket;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{error, info};

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting ROUTER worker pool on tcp://127.0.0.1:5555");

    let listener = TcpListener::bind("127.0.0.1:5555").await?;
    let task_counter = Arc::new(AtomicU64::new(0));

    info!("Waiting for worker connections...");

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("Worker connected from {addr}");

                let counter = task_counter.clone();
                compio::runtime::spawn(async move {
                    handle_worker(stream, counter).await;
                });
            }
            Err(e) => {
                error!("Accept error: {e}");
            }
        }
    }
}

async fn handle_worker(stream: compio::net::TcpStream, task_counter: Arc<AtomicU64>) {
    let mut socket = RouterSocket::from_stream(stream).await.unwrap();

    // Send tasks to this worker
    for _ in 0..10 {
        let task_id = task_counter.fetch_add(1, Ordering::SeqCst);
        let task = format!("Task #{task_id}");

        info!("Sending: {task}");

        // In ROUTER mode, first frame is routing ID (handled internally)
        // We just send the task body
        match socket.send(vec![Bytes::from(task)]).await {
            Ok(()) => {
                // Wait for completion response
                if let Some(response) = socket.recv().await {
                    if let Some(result) = response.last() {
                        if let Ok(s) = std::str::from_utf8(result) {
                            info!("Worker completed: {s}");
                        }
                    }
                } else {
                    error!("Connection closed");
                    break;
                }
            }
            Err(e) => {
                error!("Send error: {e}");
                break;
            }
        }

        // Small delay between tasks
        compio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    info!("Worker session complete");
}
