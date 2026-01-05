/// ROUTER socket implementation
///
/// Architecture: Application → RouterSocket → ZmtpIntegratedActor → SocketActor → TcpStream
///
/// ROUTER sockets receive messages with identity envelopes (first frame is sender identity)
/// and can route replies back to specific peers using that identity.

#[cfg(feature = "runtime")]
pub use runtime_impl::*;

#[cfg(feature = "runtime")]
mod runtime_impl {
    use bytes::Bytes;
    use compio::net::TcpStream;
    use flume::{Receiver, Sender, unbounded};
    use monocoque_core::{
        actor::{SocketActor, SocketEvent, UserCmd},
        alloc::IoArena,
    };
    use crate::{
        integrated_actor::ZmtpIntegratedActor,
        session::SocketType,
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
        /// Create a new ROUTER socket from a TcpStream
        ///
        /// This spawns:
        /// 1. SocketActor task for I/O
        /// 2. Integration task bridging socket events → ZMTP session
        pub async fn new(tcp_stream: TcpStream) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SOCKET_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
            let socket_id = SOCKET_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
            eprintln!("[ROUTER new()] Creating RouterSocket #{}", socket_id);
            
            // Create channels for socket actor communication
            let (socket_event_tx, socket_event_rx) = unbounded();
            let (socket_cmd_tx, socket_cmd_rx) = unbounded();

            // Create channels for application communication
            let (app_tx, app_rx) = unbounded(); // integrated → application (for recv)
            let (user_tx, user_rx) = unbounded(); // application → integrated (for send)
            
            // TEST: Verify channel pairing by sending a test message
            eprintln!("[ROUTER new()] Testing channel pairing...");
            user_tx.send(vec![Bytes::from("TEST")]).expect("Test send failed");
            match user_rx.try_recv() {
                Ok(msg) => eprintln!("[ROUTER new()] ✓ Channel pair is connected! Received test message: {:?}", msg),
                Err(e) => panic!("[ROUTER new()] ✗ CHANNEL PAIR IS BROKEN! Error: {:?}", e),
            }
            
            // CRITICAL DEBUG: Store channel ID at creation time
            let channel_pair_id = user_tx.len() as usize + std::ptr::addr_of!(user_tx) as usize;
            eprintln!("[ROUTER new()] CHANNEL PAIR ID: {} (computed from user_tx)", channel_pair_id);
            
            // Debug: Print channel identity
            eprintln!("[ROUTER new()] user_tx: {:p} (sender_count={}), user_rx: {:p} (receiver_count={})", 
                     &user_tx as *const _, user_tx.sender_count(),
                     &user_rx as *const _, user_rx.receiver_count());

            let rx_channel_id_before = user_rx.len() as usize + std::ptr::addr_of!(user_rx) as usize;
            eprintln!("[ROUTER new()] user_rx channel ID before move: {}", rx_channel_id_before);

            // Create socket actor
            let io_arena = IoArena::new();
            let socket_actor = SocketActor::new(
                tcp_stream,
                socket_event_tx,
                socket_cmd_rx,
                io_arena,
            );

            // Create ZmtpIntegratedActor
            let mut integrated_actor = ZmtpIntegratedActor::new(
                SocketType::Router,
                app_tx,      // integrated sends received messages TO app
                user_rx,     // integrated receives outgoing messages FROM app
            );

            // TEST: Verify channel still works after creating integrated_actor
            eprintln!("[ROUTER new()] Testing channel after ZmtpIntegratedActor::new...");
            user_tx.send(vec![Bytes::from("TEST2")]).expect("Test2 send failed");
            match integrated_actor.user_rx.try_recv() {
                Ok(msg) => eprintln!("[ROUTER new()] ✓ Channel STILL connected after new()! Received: {:?}", msg),
                Err(e) => panic!("[ROUTER new()] ✗ CHANNEL BROKEN AFTER new()! Error: {:?}", e),
            }

            // Send initial greeting
            let greeting = integrated_actor.local_greeting();
            let _ = socket_cmd_tx.send(UserCmd::SendBytes(greeting));

            // Spawn integration task
            let socket_cmd_tx_clone = socket_cmd_tx.clone();
            let integration_handle = compio::runtime::spawn(async move {
                let rx_id_in_task = integrated_actor.user_rx.len() as usize + std::ptr::addr_of!(integrated_actor.user_rx) as usize;
                eprintln!("[ROUTER TASK] Inside task - integrated_actor.user_rx channel ID: {}", rx_id_in_task);
                eprintln!("[ROUTER TASK] Integration task started! integrated_actor.user_rx receiver_count={}", 
                         integrated_actor.user_rx.receiver_count());
                let mut handshake_complete = false;
                let mut iteration = 0u64;
                
                loop {
                    iteration += 1;
                    if iteration % 10000 == 0 {
                        eprintln!("[ROUTER TASK] Integration loop iteration {}", iteration);
                    }
                    
                    // Process socket events
                    if let Ok(event) = socket_event_rx.try_recv() {
                        match event {
                            SocketEvent::Connected => {
                                // Connection established
                            }
                            SocketEvent::ReceivedBytes(bytes) => {
                                // Feed bytes into ZMTP session
                                let session_events = integrated_actor.session.on_bytes(bytes);
                                
                                for event in session_events {
                                    match event {
                                        crate::session::SessionEvent::SendBytes(data) => {
                                            let _ = socket_cmd_tx.send(UserCmd::SendBytes(data));
                                        }
                                        crate::session::SessionEvent::HandshakeComplete { peer_identity, peer_socket_type: _ } => {
                                            integrated_actor.handle_handshake_complete(peer_identity);
                                            handshake_complete = true;
                                            // READY command already sent by Session
                                        }
                                        crate::session::SessionEvent::Frame(frame) => {
                                            if handshake_complete {
                                                eprintln!("[ROUTER TASK] Received frame from peer, passing to integrated_actor");
                                                integrated_actor.handle_frame(frame);
                                            }
                                        }
                                        crate::session::SessionEvent::Error(_e) => {
                                            break;
                                        }
                                    }
                                }
                            }
                            SocketEvent::Disconnected => {
                                break;
                            }
                        }
                    }

                    // Process outgoing messages
                    let outgoing_frames = integrated_actor.process_events().await;
                    if !outgoing_frames.is_empty() {
                        eprintln!("[ROUTER TASK] Got {} outgoing frames from process_events()", outgoing_frames.len());
                        for frame in outgoing_frames {
                            eprintln!("[ROUTER TASK] Sending {} bytes to SocketActor", frame.len());
                            let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                        }
                    }

                    // Small yield to prevent busy-waiting and allow other tasks to run
                    compio::time::sleep(std::time::Duration::from_micros(100)).await;
                }
                eprintln!("[ROUTER TASK] Integration task exited!");
            });

            // Spawn SocketActor
            let socket_handle = compio::runtime::spawn(socket_actor.run());

            // Yield to allow spawned tasks to start
            compio::time::sleep(std::time::Duration::from_micros(100)).await;

            // TEST: Send a message AFTER integration task has started
            eprintln!("[ROUTER new()] Testing channel AFTER integration task spawn...");
            user_tx.send(vec![Bytes::from("TEST3-AFTER-SPAWN")]).expect("Test3 send failed");
            // Give the integration task time to process
            compio::time::sleep(std::time::Duration::from_millis(10)).await;

            eprintln!("[ROUTER new()] Returning RouterSocket #{}, user_tx sender_count={}", 
                     socket_id, user_tx.sender_count());
            let user_tx_id_before_move = user_tx.len() as usize + std::ptr::addr_of!(user_tx) as usize;
            eprintln!("[ROUTER new()] user_tx ID before move into RouterSocket: {}", user_tx_id_before_move);
            let socket = RouterSocket {
                app_tx: user_tx,  // app sends outgoing messages here
                app_rx,           // app receives incoming messages from here
                _task_handles: (integration_handle, socket_handle),
            };
            let socket_app_tx_id = socket.app_tx.len() as usize + std::ptr::addr_of!(socket.app_tx) as usize;
            eprintln!("[ROUTER new()] socket.app_tx ID after move: {}", socket_app_tx_id);
            eprintln!("[ROUTER new()] After creating RouterSocket, socket.app_tx sender_count={}", 
                     socket.app_tx.sender_count());
            socket
        }

        /// Send a multipart message with identity routing
        ///
        /// Format: [identity, ...message_frames]
        /// The first frame must be the peer identity to route to.
        pub async fn send(&self, msg: Vec<Bytes>) -> Result<(), flume::SendError<Vec<Bytes>>> {
            let tx_channel_id = self.app_tx.len() as usize + std::ptr::addr_of!(self.app_tx) as usize;
            eprintln!("[ROUTER send()] app_tx channel ID: {}, sender_count={}, queued before: {}", 
                     tx_channel_id, self.app_tx.sender_count(), self.app_tx.len());
            let result = self.app_tx.send_async(msg).await;
            eprintln!("[ROUTER send()] queued after: {}, result: {:?}", 
                     self.app_tx.len(), result.as_ref().map(|_| "OK"));
            result
        }

        /// Receive a multipart message with identity envelope
        ///
        /// Format: [identity, ...message_frames]
        /// The first frame is the sender's identity.
        pub async fn recv(&self) -> Result<Vec<Bytes>, flume::RecvError> {
            self.app_rx.recv_async().await
        }
    }
}
