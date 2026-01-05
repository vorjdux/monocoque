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
use monocoque_zmtp::RouterSocket;
use compio::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting ROUTER worker pool on tcp://127.0.0.1:5555");
    
    let listener = TcpListener::bind("127.0.0.1:5555").await?;
    let task_counter = Arc::new(AtomicU64::new(0));
    
    println!("Waiting for worker connections...");
    
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                println!("Worker connected from {}", addr);
                
                let counter = task_counter.clone();
                compio::runtime::spawn(async move {
                    handle_worker(stream, counter).await;
                });
            }
            Err(e) => {
                eprintln!("Accept error: {}", e);
            }
        }
    }
}

async fn handle_worker(
    stream: compio::net::TcpStream,
    task_counter: Arc<AtomicU64>,
) {
    let socket = RouterSocket::new(stream);
    
    // Send tasks to this worker
    for _ in 0..10 {
        let task_id = task_counter.fetch_add(1, Ordering::SeqCst);
        let task = format!("Task #{}", task_id);
        
        println!("Sending: {}", task);
        
        // In ROUTER mode, first frame is routing ID (handled internally)
        // We just send the task body
        match socket.send(vec![Bytes::from(task)]).await {
            Ok(_) => {
                // Wait for completion response
                match socket.recv().await {
                    Ok(response) => {
                        if let Some(result) = response.last() {
                            if let Ok(s) = std::str::from_utf8(result) {
                                println!("Worker completed: {}", s);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Receive error: {}", e);
                        break;
                    }
                }
            }
            Err(e) => {
                eprintln!("Send error: {}", e);
                break;
            }
        }
        
        // Small delay between tasks
        compio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    
    println!("Worker session complete");
}
