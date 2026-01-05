/// Complete Publish-Subscribe Pattern Example
/// 
/// Demonstrates PUB (publisher) and SUB (subscriber) pattern
/// This is a common pattern for event distribution

#[cfg(feature = "runtime")]
fn main() {
    use monocoque_zmtp::publisher::PubSocket;
    use monocoque_zmtp::subscriber::SubSocket;
    use bytes::Bytes;
    use std::thread;

    println!("=== Publish-Subscribe Pattern ===\n");
    println!("Publisher: PUB socket (broadcasts to all)");
    println!("Subscriber: SUB socket (filters by topic)\n");

    // Publisher thread
    let publisher = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            println!("[Publisher] Starting on 127.0.0.1:9001...");
            let listener = compio::net::TcpListener::bind("127.0.0.1:9001").await.unwrap();
            
            println!("[Publisher] Waiting for subscribers...");
            let (stream, addr) = listener.accept().await.unwrap();
            println!("[Publisher] Subscriber connected from {}", addr);
            
            let mut pub_socket = PubSocket::new(stream);
            
            // Give subscriber time to set up
            compio::time::sleep(std::time::Duration::from_millis(100)).await;
            
            // Publish messages on different topics
            let messages = vec![
                ("weather.sunny", "Temperature: 72°F"),
                ("weather.cloudy", "Overcast conditions"),
                ("news.tech", "Rust 2.0 announced!"),
                ("weather.rainy", "Precipitation: 80%"),
                ("news.sports", "Team wins championship"),
            ];
            
            for (topic, body) in messages {
                println!("[Publisher] Publishing: {} -> {}", topic, body);
                
                pub_socket.send(vec![
                    Bytes::from(topic),
                    Bytes::from(body),
                ]).await.unwrap();
                
                compio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            
            println!("[Publisher] Done publishing");
        });
    });

    // Give publisher time to start
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Subscriber thread
    let subscriber = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            println!("[Subscriber] Connecting to publisher...");
            let stream = compio::net::TcpStream::connect("127.0.0.1:9001").await.unwrap();
            println!("[Subscriber] Connected!");
            
            let sub_socket = SubSocket::new(stream);
            
            // Subscribe only to weather updates
            println!("[Subscriber] Subscribing to 'weather.' topic");
            sub_socket.subscribe(b"weather.").await.unwrap();
            
            // Receive messages (only weather topics will come through)
            let mut count = 0;
            while count < 3 {
                let msg = sub_socket.recv().await.unwrap();
                let topic = String::from_utf8_lossy(&msg[0]);
                let body = String::from_utf8_lossy(&msg[1]);
                
                println!("[Subscriber] Received: {} -> {}", topic, body);
                count += 1;
            }
            
            println!("[Subscriber] Done! (Filtered out non-weather messages)");
        });
    });

    publisher.join().unwrap();
    subscriber.join().unwrap();
    
    println!("\n✅ Publish-Subscribe pattern complete!");
    println!("\nThis demonstrates:");
    println!("- PUB socket broadcasting messages to all subscribers");
    println!("- SUB socket filtering messages by topic prefix");
    println!("- Subscribe/unsubscribe API");
    println!("- One-to-many communication pattern");
}

#[cfg(not(feature = "runtime"))]
fn main() {
    println!("This example requires the 'runtime' feature.");
    println!("Run with: cargo run --example pubsub --features runtime");
}
