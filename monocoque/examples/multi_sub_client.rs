/// Simple subscriber client for testing multi-subscriber publisher
use monocoque::rt::LocalRuntime;
use monocoque::zmq::SubSocket;
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let subscriber_id = std::env::args().nth(1).unwrap_or_else(|| "1".to_string());

    info!("[SUB-{}] Connecting to publisher...", subscriber_id);

    let mut sub_socket = SubSocket::connect("127.0.0.1:5556").await?;

    // Subscribe to all messages starting with "test."
    sub_socket.subscribe(b"test.").await?;
    info!("[SUB-{}] Subscribed to 'test.*'", subscriber_id);

    // Receive messages
    let mut count = 0;
    loop {
        if let Some(frames) = sub_socket.recv().await? {
            count += 1;
            let topic = String::from_utf8_lossy(&frames[0]);
            let data = if frames.len() > 1 {
                String::from_utf8_lossy(&frames[1])
            } else {
                "".into()
            };
            info!(
                "[SUB-{}] Received #{}: topic='{}' data='{}'",
                subscriber_id, count, topic, data
            );
        } else {
            info!("[SUB-{}] Connection closed", subscriber_id);
            break;
        }
    }

    Ok(())
}
