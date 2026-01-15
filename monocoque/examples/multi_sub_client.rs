/// Simple subscriber client for testing multi-subscriber publisher
use monocoque::zmq::SubSocket;
use tracing::info;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let subscriber_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1".to_string());
    
    info!("[SUB-{}] Connecting to publisher...", subscriber_id);
    
    let mut sub_socket = SubSocket::connect("127.0.0.1:5556").await?;
    
    // Subscribe to all messages starting with "test."
    sub_socket.subscribe(b"test.").await?;
    info!("[SUB-{}] Subscribed to 'test.*'", subscriber_id);
    
    // Receive messages
    let mut count = 0;
    loop {
        match sub_socket.recv().await? {
            Some(frames) => {
                count += 1;
                let topic = String::from_utf8_lossy(&frames[0]);
                let data = if frames.len() > 1 {
                    String::from_utf8_lossy(&frames[1])
                } else {
                    "".into()
                };
                info!("[SUB-{}] Received #{}: topic='{}' data='{}'", 
                      subscriber_id, count, topic, data);
            }
            None => {
                info!("[SUB-{}] Connection closed", subscriber_id);
                break;
            }
        }
    }
    
    Ok(())
}
