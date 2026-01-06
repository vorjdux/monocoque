use crate::{integrated_actor::ZmtpIntegratedActor, session::SocketType};
/// PUB (Publisher) socket implementation
///
/// Architecture: Application → `PubSocket` → `ZmtpIntegratedActor` → `SocketActor` → `TcpStream`
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
    _task_handles: (compio::runtime::Task<()>, compio::runtime::Task<()>),
}

impl PubSocket {
    /// Create a new PUB socket from a `TcpStream`
    ///
    /// This spawns:
    /// 1. `SocketActor` task for I/O
    /// 2. Integration task bridging socket events → ZMTP session
    pub async fn new(tcp_stream: TcpStream) -> Self {
        // Create channels for socket actor communication
        let (socket_event_tx, socket_event_rx) = unbounded();
        let (socket_cmd_tx, socket_cmd_rx) = unbounded();

        // Create channels for application communication
        let (user_tx, user_rx) = unbounded(); // application → integrated (for send)
        let (integrated_tx, integrated_rx) = unbounded(); // integrated → application (for recv, unused in PUB)

        // Create socket actor
        let io_arena = IoArena::new();
        let socket_actor = SocketActor::new(tcp_stream, socket_event_tx, socket_cmd_rx, io_arena);

        // Create ZmtpIntegratedActor
        let mut integrated_actor =
            ZmtpIntegratedActor::new(SocketType::Pub, integrated_tx, user_rx);

        // Send initial greeting before spawning tasks
        let greeting = integrated_actor.local_greeting();
        let _ = socket_cmd_tx.send(UserCmd::SendBytes(greeting));

        // Spawn SocketActor first
        let socket_handle = compio::runtime::spawn(socket_actor.run());

        // Small delay to allow SocketActor pumps to initialize
        // TODO: Replace with proper synchronization mechanism
        compio::time::sleep(std::time::Duration::from_millis(1)).await;

        // Spawn integration task
        let integration_handle = compio::runtime::spawn(async move {
            eprintln!("[PUB TASK] Integration task started");

            use futures::{select, FutureExt};

            let mut handshake_complete = false;

            loop {
                select! {
                    // Wait for socket events
                    event = socket_event_rx.recv_async().fuse() => {
                        match event {
                            Ok(SocketEvent::Connected) => {
                                // Connection established
                            }
                            Ok(SocketEvent::ReceivedBytes(bytes)) => {
                                // Feed bytes into ZMTP session
                                let session_events = integrated_actor.session.on_bytes(bytes);

                                for event in session_events {
                                    match event {
                                        crate::session::SessionEvent::SendBytes(data) => {
                                            let _ = socket_cmd_tx.send(UserCmd::SendBytes(data));
                                        }
                                        crate::session::SessionEvent::HandshakeComplete {
                                            peer_identity,
                                            peer_socket_type: _,
                                        } => {
                                            integrated_actor.handle_handshake_complete(peer_identity);
                                            handshake_complete = true;
                                        }
                                        crate::session::SessionEvent::Frame(frame) => {
                                            if handshake_complete {
                                                integrated_actor.handle_frame(frame);
                                            }
                                        }
                                        crate::session::SessionEvent::Error(_e) => {
                                            break;
                                        }
                                    }
                                }
                            }
                            Ok(SocketEvent::Disconnected) | Err(_) => {
                                break;
                            }
                        }
                    }
                    // Wait for outgoing messages from application
                    msg = integrated_actor.user_rx.recv_async().fuse() => {
                        match msg {
                            Ok(multipart) => {
                                eprintln!("[PUB TASK] Got {} frames from user_rx", multipart.len());
                                let frames = integrated_actor.encode_outgoing_message(multipart);
                                for frame in frames {
                                    eprintln!("[PUB TASK] Sending {} bytes to SocketActor", frame.len());
                                    let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                                }
                            }
                            Err(_) => {
                                eprintln!("[PUB TASK] user_rx channel closed, exiting");
                                break;
                            }
                        }
                    }
                }
            }
            eprintln!("[PUB TASK] Integration task exiting");
        });

        Self {
            app_tx: user_tx,
            _task_handles: (integration_handle, socket_handle),
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
