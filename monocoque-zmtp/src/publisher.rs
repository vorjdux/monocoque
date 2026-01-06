use crate::{integrated_actor::ZmtpIntegratedActor, session::SocketType};
/// PUB (Publisher) socket implementation
///
/// Architecture: Application → PubSocket → ZmtpIntegratedActor → SocketActor → TcpStream
///
/// PUB sockets broadcast messages to all connected SUB sockets.
use bytes::Bytes;
use compio::net::TcpStream;
use flume::{unbounded, Sender};
use monocoque_core::{
    actor::{SocketActor, SocketEvent, UserCmd},
    alloc::IoArena,
};

/// High-level PUB socket with async send API
///
/// Messages are multipart and broadcast to all subscribers.
pub struct PubSocket {
    app_tx: Sender<Vec<Bytes>>,
}

impl PubSocket {
    /// Create a new PUB socket from a TcpStream
    ///
    /// This spawns:
    /// 1. SocketActor task for I/O
    /// 2. Integration task bridging socket events → ZMTP session
    pub fn new(tcp_stream: TcpStream) -> Self {
        // Create channels for socket actor communication
        let (socket_event_tx, socket_event_rx) = unbounded();
        let (socket_cmd_tx, socket_cmd_rx) = unbounded();

        // Create channels for application communication
        let (app_tx, app_rx_for_integrated) = unbounded();
        let (app_tx_for_user, _app_rx_unused) = unbounded();

        // Create socket actor
        let io_arena = IoArena::new();
        let socket_actor = SocketActor::new(tcp_stream, socket_event_tx, socket_cmd_rx, io_arena);

        // Create ZmtpIntegratedActor
        let mut integrated_actor =
            ZmtpIntegratedActor::new(SocketType::Pub, app_tx.clone(), app_rx_for_integrated);

        // Send initial greeting
        let greeting = integrated_actor.local_greeting();
        let _ = socket_cmd_tx.send(UserCmd::SendBytes(greeting));

        // Spawn integration task
        let _handle = compio::runtime::spawn(async move {
            loop {
                // Process socket events (mostly for control messages)
                if let Ok(event) = socket_event_rx.try_recv() {
                    match event {
                        SocketEvent::Connected => {
                            // Connection established
                        }
                        SocketEvent::ReceivedBytes(bytes) => {
                            // Feed bytes into ZMTP session
                            let frames = integrated_actor.on_bytes(bytes);

                            // Send frames back to socket
                            for frame in frames {
                                let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                            }
                        }
                        SocketEvent::Disconnected => {
                            break;
                        }
                    }
                }

                // Process outgoing messages
                let outgoing_frames = integrated_actor.process_events().await;
                for frame in outgoing_frames {
                    let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                }

                // Small yield to prevent busy-waiting
                compio::time::sleep(std::time::Duration::from_micros(100)).await;
            }
        });

        // Spawn SocketActor
        let _ = compio::runtime::spawn(socket_actor.run());

        PubSocket {
            app_tx: app_tx_for_user,
        }
    }

    /// Send (broadcast) a multipart message to all subscribers
    ///
    /// The message will be distributed to all connected SUB sockets
    /// that have matching subscriptions.
    pub async fn send(&self, msg: Vec<Bytes>) -> Result<(), flume::SendError<Vec<Bytes>>> {
        self.app_tx.send_async(msg).await
    }
}
