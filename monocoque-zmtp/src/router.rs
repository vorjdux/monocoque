use crate::{integrated_actor::ZmtpIntegratedActor, session::SocketType};
/// ROUTER socket implementation
///
/// Architecture: Application → `RouterSocket` → `ZmtpIntegratedActor` → `SocketActor` → `TcpStream`
///
/// ROUTER sockets receive messages with identity envelopes (first frame is sender identity)
/// and can route replies back to specific peers using that identity.
use bytes::Bytes;
use compio::net::TcpStream;
use flume::{unbounded, Receiver, Sender};
use monocoque_core::{
    actor::{SocketActor, SocketEvent, UserCmd},
    alloc::IoArena,
};

/// High-level ROUTER socket with async send/recv API
///
/// Messages are multipart: [identity, ...frames]
/// The first frame is always the peer identity for routing.
pub struct RouterSocket {
    app_tx: Sender<Vec<Bytes>>,
    app_rx: Receiver<Vec<Bytes>>,
    _task_handles: (compio::runtime::Task<()>, compio::runtime::Task<()>),
}

impl RouterSocket {
    /// Create a new ROUTER socket from a `TcpStream`.
    ///
    /// **Internal API**: For public-facing ergonomics, use `monocoque::RouterSocket::bind()`.
    ///
    /// This spawns I/O and integration tasks.
    /// 
    /// Performs synchronous handshake before spawning tasks to eliminate race conditions.
    pub async fn new(mut tcp_stream: TcpStream) -> Self {
        use crate::handshake::perform_handshake;

        // Perform synchronous handshake FIRST (before spawning any tasks)
        let handshake_result = perform_handshake(&mut tcp_stream, SocketType::Router, None)
            .await
            .expect("Handshake failed");
        
        // ROUTER: Generate routing ID if peer didn't provide one
        let peer_identity = handshake_result.peer_identity.or_else(|| {
            // Generate a unique ID based on connection (e.g., using TCP peer address)
            // For now, use a simple counter-based approach
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(1);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            Some(Bytes::from(format!("peer-{}", id)))
        });

        // Create channels for socket actor communication
        let (socket_event_tx, socket_event_rx) = unbounded();
        let (socket_cmd_tx, socket_cmd_rx) = unbounded();

        // Create channels for application communication
        let (app_tx, app_rx) = unbounded(); // integrated → application (for recv)
        let (user_tx, user_rx) = unbounded(); // application → integrated (for send)

        // Create socket actor with post-handshake stream
        let io_arena = IoArena::new();
        let socket_actor = SocketActor::new(tcp_stream, socket_event_tx, socket_cmd_rx, io_arena);

        // Create ZmtpIntegratedActor in Active state (handshake already complete)
        let mut integrated_actor = ZmtpIntegratedActor::new_active(
            SocketType::Router,
            app_tx,  // integrated sends received messages TO app
            user_rx, // integrated receives outgoing messages FROM app
            peer_identity,
        );

        // Spawn SocketActor
        let socket_handle = compio::runtime::spawn(socket_actor.run());

        // Spawn integration task
        let _socket_cmd_tx_clone = socket_cmd_tx.clone();
        let integration_handle = compio::runtime::spawn(async move {
            let rx_id_in_task = integrated_actor.user_rx.len()
                + std::ptr::addr_of!(integrated_actor.user_rx) as usize;
            eprintln!(
                "[ROUTER TASK] Inside task - integrated_actor.user_rx channel ID: {rx_id_in_task}"
            );
            eprintln!("[ROUTER TASK] Integration task started! integrated_actor.user_rx receiver_count={}", 
                         integrated_actor.user_rx.receiver_count());

            // Handshake already complete, session is in Active state
            use futures::{select, FutureExt};

            loop {
                select! {
                    // Wait for socket events (bytes from network)
                    event = socket_event_rx.recv_async().fuse() => {
                        match event {
                            Ok(SocketEvent::Connected) => {
                                // Connection established
                            }
                            Ok(SocketEvent::ReceivedBytes(bytes)) => {
                                // Feed bytes into ZMTP session (only data frames now, no handshake)
                                let session_events = integrated_actor.session.on_bytes(bytes);

                                for event in session_events {
                                    match event {
                                        crate::session::SessionEvent::SendBytes(data) => {
                                            let _ = socket_cmd_tx.send(UserCmd::SendBytes(data));
                                        }
                                        crate::session::SessionEvent::Frame(frame) => {
                                            eprintln!("[ROUTER TASK] Received frame from peer, passing to integrated_actor");
                                            integrated_actor.handle_frame(frame);
                                        }
                                        crate::session::SessionEvent::Error(_e) => {
                                            break;
                                        }
                                        _ => {
                                            // HandshakeComplete shouldn't happen since we're in Active state
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
                                eprintln!("[ROUTER TASK] Got {} frames from user_rx", multipart.len());
                                let frames = integrated_actor.encode_outgoing_message(multipart);
                                for frame in frames {
                                    eprintln!("[ROUTER TASK] Sending {} bytes to SocketActor", frame.len());
                                    let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                                }
                            }
                            Err(_) => {
                                // Channel closed
                                break;
                            }
                        }
                    }
                }
            }

            eprintln!("[ROUTER TASK] Integration task exited!");
        });

        Self {
            app_tx: user_tx,
            app_rx,
            _task_handles: (integration_handle, socket_handle),
        }
    }

    /// Send a multipart message with identity routing
    ///
    /// Format: [identity, ...`message_frames`]
    /// The first frame must be the peer identity to route to.
    pub async fn send(&self, msg: Vec<Bytes>) -> Result<(), flume::SendError<Vec<Bytes>>> {
        let tx_channel_id = self.app_tx.len() + std::ptr::addr_of!(self.app_tx) as usize;
        eprintln!(
            "[ROUTER send()] app_tx channel ID: {}, sender_count={}, queued before: {}",
            tx_channel_id,
            self.app_tx.sender_count(),
            self.app_tx.len()
        );
        let result = self.app_tx.send_async(msg).await;
        eprintln!(
            "[ROUTER send()] queued after: {}, result: {:?}",
            self.app_tx.len(),
            result.as_ref().map(|()| "OK")
        );
        result
    }

    /// Receive a multipart message with identity envelope
    ///
    /// Format: [identity, ...`message_frames`]
    /// The first frame is the sender's identity.
    pub async fn recv(&self) -> Result<Vec<Bytes>, flume::RecvError> {
        self.app_rx.recv_async().await
    }
}
