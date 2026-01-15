// Minimal test of multi-subscriber PUB socket with worker pool
use monocoque::zmq::{Context, Socket};
use monocoque::SocketType;
use std::thread;
use std::time::Duration;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    let ctx = Context::new();
    let mut pub_socket = ctx.socket(SocketType::Pub)?;
    
    // Bind to localhost
    pub_socket.bind("tcp://127.0.0.1:5556").await?;
    println!("Publisher bound to tcp://127.0.0.1:5556");
    println!("Using worker pool with {} workers", num_cpus::get());
    
    // Give time for subscribers to connect
    thread::sleep(Duration::from_secs(1));
    
    // Send a few messages
    for i in 0..10 {
        let msg = format!("Message {}", i);
        pub_socket.send(&[msg.as_bytes()]).await?;
        println!("Sent: {}", msg);
        thread::sleep(Duration::from_millis(100));
    }
    
    println!("Publisher done");
    Ok(())
}
