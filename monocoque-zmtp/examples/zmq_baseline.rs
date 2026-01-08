//! zmq.rs baseline for comparison

use std::time::Instant;

fn main() {
    let ctx = zmq::Context::new();
    
    // Server
    let server = ctx.socket(zmq::REP).unwrap();
    server.bind("tcp://127.0.0.1:*").unwrap();
    let endpoint = server.get_last_endpoint().unwrap().unwrap();
    
    std::thread::spawn(move || {
        for _ in 0..10 {
            let msg = server.recv_bytes(0).unwrap();
            server.send(&msg, 0).unwrap();
        }
    });
    
    std::thread::sleep(std::time::Duration::from_millis(10));
    
    // Client
    let client = ctx.socket(zmq::REQ).unwrap();
    client.connect(&endpoint).unwrap();
    
    let payload = vec![0u8; 64];
    
    println!("zmq.rs: Measuring 10 round-trips...");
    
    let start = Instant::now();
    for _ in 0..10 {
        client.send(&payload, 0).unwrap();
        client.recv_bytes(0).unwrap();
    }
    let elapsed = start.elapsed();
    
    println!("\nTotal: {:?}", elapsed);
    println!("Per message: {:?}", elapsed / 10);
}
