use crate::{integrated_actor::ZmtpIntegratedActor, session::SocketType};
/// SUB (Subscriber) socket implementation
///
/// Architecture: Application → SubSocket → ZmtpIntegratedActor → SocketActor → TcpStream
///
/// SUB sockets receive messages from PUB sockets based on subscriptions.
use bytes::Bytes;
use compio::net::TcpStream;
use flume::{unbounded, Receiver, Sender};
use monocoque_core::{
    actor::{SocketActor, SocketEvent, UserCmd},
    alloc::IoArena,
};

/// High-level SUB socket with async recv API and subscription management
///
/// Messages are received based on active subscriptions.
pub struct SubSocket {
    app_rx: Receiver<Vec<Bytes>>,
    socket_cmd_tx: Sender<UserCmd>,
}

impl SubSocket {
    /// Create a new SUB socket from a TcpStream
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
        let (_app_tx_unused, app_rx) = unbounded();

        // Create socket actor
        let io_arena = IoArena::new();
        let socket_actor = SocketActor::new(tcp_stream, socket_event_tx, socket_cmd_rx, io_arena);

        // Create ZmtpIntegratedActor
        let mut integrated_actor =
            ZmtpIntegratedActor::new(SocketType::Sub, app_tx, app_rx_for_integrated);

        // Send initial greeting
        let greeting = integrated_actor.local_greeting();
        let _ = socket_cmd_tx.send(UserCmd::SendBytes(greeting));

        // Clone for the closure
        let socket_cmd_tx_clone = socket_cmd_tx.clone();

        // Spawn integration task
        let _handle = compio::runtime::spawn(async move {
            loop {
                // Process socket events (incoming bytes → frames → multipart)
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
                                let _ = socket_cmd_tx_clone.send(UserCmd::SendBytes(frame));
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
                    let _ = socket_cmd_tx_clone.send(UserCmd::SendBytes(frame));
                }

                // Small yield to prevent busy-waiting
                compio::time::sleep(std::time::Duration::from_micros(100)).await;
            }
        });

        // Spawn SocketActor
        let _ = compio::runtime::spawn(socket_actor.run());

        SubSocket {
            app_rx,
            socket_cmd_tx,
        }
    }

    /// Subscribe to messages matching the given topic prefix
    ///
    /// Empty topic subscribes to all messages.
    ///
    /// Note: This sends a SUBSCRIBE command frame through the socket.
    pub async fn subscribe(&self, topic: &[u8]) -> Result<(), flume::SendError<UserCmd>> {
        // Create SUBSCRIBE command frame (command type 0x01 + topic)
        let mut cmd = Vec::with_capacity(1 + topic.len());
        cmd.push(0x01); // SUBSCRIBE command
        cmd.extend_from_slice(topic);

        self.socket_cmd_tx
            .send_async(UserCmd::SendBytes(Bytes::from(cmd)))
            .await
    }

    /// Unsubscribe from messages matching the given topic prefix
    pub async fn unsubscribe(&self, topic: &[u8]) -> Result<(), flume::SendError<UserCmd>> {
        // Create UNSUBSCRIBE command frame (command type 0x00 + topic)
        let mut cmd = Vec::with_capacity(1 + topic.len());
        cmd.push(0x00); // UNSUBSCRIBE command
        cmd.extend_from_slice(topic);

        self.socket_cmd_tx
            .send_async(UserCmd::SendBytes(Bytes::from(cmd)))
            .await
    }

    /// Receive a multipart message that matches active subscriptions
    pub async fn recv(&self) -> Result<Vec<Bytes>, flume::RecvError> {
        self.app_rx.recv_async().await
    }
}
