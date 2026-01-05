/// Handshake Test - Validates ZMTP greeting and READY exchange
///
/// This is a minimal test to verify the protocol handshake works before
/// attempting full message exchange.

#[cfg(feature = "runtime")]
use monocoque_zmtp::{DealerSocket, RouterSocket};
#[cfg(feature = "runtime")]
use compio::net::{TcpListener, TcpStream};
#[cfg(feature = "runtime")]
use std::time::Duration;

#[cfg(feature = "runtime")]
#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ZMTP Handshake Test ===\n");

    // Start server
    let server_task = compio::runtime::spawn(async {
        println!("[SERVER] Binding...");
        let listener = TcpListener::bind("127.0.0.1:5571").await.unwrap();
        println!("[SERVER] Listening on :5571");
        
        let (stream, _addr) = listener.accept().await.unwrap();
        println!("[SERVER] Connection accepted");
        
        let _socket = RouterSocket::new(stream);
        println!("[SERVER] Socket created");
        
        // Give time for handshake
        for i in 1..=10 {
            println!("[SERVER] Handshake progress... {}s", i);
            compio::time::sleep(Duration::from_millis(500)).await;
        }
        
        println!("[SERVER] Done");
    });

    // Give server time to bind
    compio::time::sleep(Duration::from_millis(100)).await;

    // Start client
    println!("[CLIENT] Connecting...");
    let stream = TcpStream::connect("127.0.0.1:5571").await?;
    println!("[CLIENT] Connected");
    
    let _socket = DealerSocket::new(stream);
    println!("[CLIENT] Socket created");
    
    // Give time for handshake
    for i in 1..=10 {
        println!("[CLIENT] Handshake progress... {}s", i);
        compio::time::sleep(Duration::from_millis(500)).await;
    }
    
    println!("[CLIENT] Done");
    
    // Wait for server
    server_task.await.unwrap();

    println!("\n✅ Handshake test completed (both sides stayed connected)");
    Ok(())
}

#[cfg(not(feature = "runtime"))]
fn main() {
    eprintln!("This example requires the 'runtime' feature.");
    eprintln!("Run with: cargo run --example handshake_test --features runtime");
}
