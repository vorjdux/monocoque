/// Complete Publish-Subscribe Pattern Example
///
/// Demonstrates PUB (publisher) and SUB (subscriber) pattern
/// This is a common pattern for event distribution

fn main() {
    use bytes::Bytes;
    use monocoque_zmtp::publisher::PubSocket;
    use monocoque_zmtp::subscriber::SubSocket;
    use std::thread;
    use tracing::info;

    info!("=== Publish-Subscribe Pattern ===\n");
    info!("Publisher: PUB socket (broadcasts to all)");
    info!("Subscriber: SUB socket (filters by topic)\n");

    // Publisher thread
    let publisher = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            info!("[Publisher] Starting on 127.0.0.1:9001...");
            let listener = compio::net::TcpListener::bind("127.0.0.1:9001")
                .await
                .unwrap();

            info!("[Publisher] Waiting for subscribers...");
            let (stream, addr) = listener.accept().await.unwrap();
            info!("[Publisher] Subscriber connected from {addr}");

            let mut pub_socket = PubSocket::new(stream).await.unwrap();

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
                info!("[Publisher] Publishing: {topic} -> {body}");

                match pub_socket
                    .send(vec![Bytes::from(topic), Bytes::from(body)])
                    .await
                {
                    Ok(_) => {}
                    Err(_) => {
                        info!("[Publisher] Subscriber disconnected, stopping");
                        break;
                    }
                }

                compio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            info!("[Publisher] Done publishing");
        });
    });

    // Give publisher time to start
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Subscriber thread
    let subscriber = thread::spawn(|| {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            info!("[Subscriber] Connecting to publisher...");
            let stream = compio::net::TcpStream::connect("127.0.0.1:9001")
                .await
                .unwrap();
            info!("[Subscriber] Connected!");

            let mut sub_socket = SubSocket::new(stream).await.unwrap();

            // Subscribe only to weather updates
            info!("[Subscriber] Subscribing to 'weather.' topic");
            sub_socket.subscribe(Bytes::from_static(b"weather."));

            // Receive messages (only weather topics will come through)
            let mut count = 0;
            while count < 3 {
                match sub_socket.recv().await {
                    Ok(Some(msg)) => {
                        let topic = String::from_utf8_lossy(&msg[0]);
                        let body = String::from_utf8_lossy(&msg[1]);

                        info!("[Subscriber] Received: {topic} -> {body}");
                        count += 1;
                    }
                    Ok(None) => {
                        info!("[Subscriber] Connection closed");
                        break;
                    }
                    Err(_) => {
                        info!("[Subscriber] Connection closed");
                        break;
                    }
                }
            }

            info!("[Subscriber] Done! (Filtered out non-weather messages)");
        });
    });

    publisher.join().expect("Publisher thread panicked");
    subscriber.join().expect("Subscriber thread panicked");

    info!("\n✅ Publish-Subscribe pattern complete!");
    info!("\nThis demonstrates:");
    info!("- PUB socket broadcasting messages to all subscribers");
    info!("- SUB socket filtering messages by topic prefix");
    info!("- Subscribe/unsubscribe API");
    info!("- One-to-many communication pattern");
}
