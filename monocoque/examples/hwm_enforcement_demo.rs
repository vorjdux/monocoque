//! Demonstration of HWM (High Water Mark) enforcement in DealerSocket
//!
//! This example shows how send_hwm prevents unbounded message buffering by
//! returning an error when the limit is reached.
//!
//! Run with: cargo run --package monocoque --features zmq --example hwm_enforcement_demo

use monocoque::zmq::{DealerSocket, RouterSocket, SocketOptions};
use bytes::Bytes;
use std::io::ErrorKind;
use compio::net::{TcpListener, TcpStream};

#[compio::main]
async fn main() -> std::io::Result<()> {
    println!("🔧 HWM Enforcement Demo\n");
    
    // Configure socket with MongoDB-style composable options
    let options = SocketOptions::default()
        .with_send_hwm(5)              // Limit: 5 buffered messages
        .with_buffer_sizes(8192, 8192); // 8KB read/write buffers
    
    println!("📊 Configuration:");
    println!("  send_hwm: {} messages", options.send_hwm);
    println!("  read_buffer: {} bytes", options.read_buffer_size);
    println!("  write_buffer: {} bytes\n", options.write_buffer_size);
    
    // Setup: Create TCP server/client pair
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    
    // Server consumes messages slowly to create backpressure
    compio::runtime::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            if let Ok(mut router) = RouterSocket::from_tcp(stream).await {
                let mut count = 0;
                while let Some(_) = router.recv().await {
                    count += 1;
                    if count % 5 == 0 {
                        println!("   [SERVER] Received {} messages", count);
                    }
                    // Simulate slow processing
                    compio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    }).detach();
    
    compio::time::sleep(std::time::Duration::from_millis(50)).await;
    
    // Connect client
    let client = TcpStream::connect(addr).await?;
    let mut dealer = DealerSocket::with_options(client, options).await?;
    
    println!("📤 Sending messages (HWM = 5)...\n");
    
    let mut sent = 0;
    
    // Send messages until HWM blocks us
    for i in 0..15 {
        let msg = vec![Bytes::from(format!("Message {}", i))];
        
        match dealer.send_buffered(msg) {
            Ok(()) => {
                sent += 1;
                print!("✓");
                if sent % 5 == 0 {
                    println!(" [{}]", sent);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock ||
                      e.to_string().contains("water mark") => {
                println!("\n\n⚠️  HWM ENFORCED: Blocked after {} messages", sent);
                println!("✅ Socket correctly prevented message #{} from buffering", i);
                println!("\n💡 Application must either:");
                println!("   - Call flush() to send buffered messages");
                println!("   - Drop messages");
                println!("   - Wait for network to drain");
                break;
            }
            Err(e) => {
                println!("\n❌ Unexpected error: {}", e);
                break;
            }
        }
    }
    
    println!("\n📊 Results:");
    println!("  Messages buffered: {}", sent);
    println!("  HWM limit: 5");
    println!("  Status: {}", if sent == 5 { "✅ PASS" } else { "❌ FAIL" });
    
    println!("\n🎯 Demo complete!");
    println!("   HWM enforcement prevents unbounded memory growth");
    println!("   Applications have explicit control over buffering");
    
    Ok(())
}
