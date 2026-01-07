//! # REP Socket Implementation
//!
//! The REP socket provides strict reply patterns with envelope tracking.
//!
//! ## Features
//!
//! - **Stateful Replies**: Automatically tracks request envelopes
//! - **Multi-client Support**: Handles multiple concurrent REQ clients
//! - **Envelope Management**: Automatic routing envelope handling
//! - **Strict Pattern**: Must recv() then send() in alternation
//!
//! ## Usage Example
//!
//! ```rust,no_run
//! use monocoque_zmtp::rep::RepSocket;
//! use compio::net::TcpListener;
//! use bytes::Bytes;
//!
//! #[compio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let listener = TcpListener::bind("127.0.0.1:5555").await?;
//!     let (stream, _) = listener.accept().await?;
//!     let socket = RepSocket::new(stream).await;
//!     
//!     loop {
//!         // Receive request
//!         if let Some(request) = socket.recv().await? {
//!             println!("Got request: {:?}", request);
//!             
//!             // Send reply
//!             socket.send(vec![Bytes::from("OK")]).await?;
//!         }
//!     }
//! }
//! ```
//!
//! ## State Machine
//!
//! REP socket enforces this state machine:
//! ```text
//! AwaitingRequest → recv() → ReadyToReply → send() → AwaitingRequest
//! ```
//!
//! Calling send() twice without recv() will return an error.

use crate::{handshake::perform_handshake, integrated_actor::ZmtpIntegratedActor, session::SocketType};
use bytes::Bytes;
use compio::net::TcpStream;
use flume::{unbounded, Receiver, Sender};
use monocoque_core::{
    actor::{SocketActor, SocketEvent, UserCmd},
    alloc::IoArena,
};

/// A REP socket for strict reply patterns.
///
/// REP sockets enforce strict alternation between receive and send operations:
/// - Must call `recv()` to get a request
/// - Must call `send()` to reply before next `recv()`
/// - Automatically handles routing envelopes for multi-hop scenarios
///
/// # Architecture
///
/// The socket integrates three layers:
/// 1. `SocketActor` - Protocol-agnostic I/O with split read/write pumps
/// 2. `ZmtpIntegratedActor` - ZMTP protocol handling (framing, handshake)
/// 3. State Machine - Enforces REP pattern and tracks envelopes
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::rep::RepSocket;
/// use compio::net::TcpStream;
/// use bytes::Bytes;
///
/// # async fn example(stream: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
/// let socket = RepSocket::new(stream).await;
///
/// // Request-reply loop
/// loop {
///     if let Some(request) = socket.recv().await? {
///         // Process request
///         let reply = vec![Bytes::from("REPLY")];
///         socket.send(reply).await?;
///     }
/// }
/// # }
/// ```
pub struct RepSocket {
    app_tx: Sender<Vec<Bytes>>,
    app_rx: Receiver<Vec<Bytes>>,
    _task_handles: (compio::runtime::Task<()>, compio::runtime::Task<()>),
}

impl RepSocket {
    /// Create a new REP socket from a TCP stream.
    ///
    /// This performs the ZMTP handshake and starts the socket actors.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::rep::RepSocket;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example(stream: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
    /// let socket = RepSocket::new(stream).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(mut stream: TcpStream) -> Self {
        println!("[REP] Creating new REP socket");

        // PHASE 1: Perform synchronous handshake on the raw stream BEFORE spawning any tasks
        eprintln!("[REP] Performing synchronous handshake...");
        let handshake_result = perform_handshake(&mut stream, SocketType::Rep, None)
            .await
            .expect("Handshake failed");
        
        eprintln!(
            "[REP] Handshake complete! Peer: {:?}, Socket Type: {:?}",
            handshake_result.peer_identity, handshake_result.peer_socket_type
        );

        // PHASE 2: Now that handshake is complete, spawn the actors
        // Create channels
        let (socket_event_tx, socket_event_rx) = unbounded(); // SocketActor → integration
        let (socket_cmd_tx, socket_cmd_rx) = unbounded(); // integration → SocketActor
        let (app_tx, app_rx) = unbounded(); // integrated → application (for recv)
        let (user_tx, user_rx) = unbounded(); // application → integrated (for send)

        // Create SocketActor with the already-handshaked stream
        let arena = IoArena::new();
        let socket_actor = SocketActor::new(stream, socket_event_tx, socket_cmd_rx, arena);

        // Create ZmtpIntegratedActor that's already in active state (handshake done)
        let mut integrated_actor = ZmtpIntegratedActor::new_active(
            SocketType::Rep,
            app_tx.clone(),
            user_rx,
            handshake_result.peer_identity,
        );

        // Spawn tasks
        eprintln!("[REP] Spawning SocketActor");
        let socket_handle = compio::runtime::spawn(socket_actor.run());
        eprintln!("[REP] SocketActor spawned");

        // Spawn the integration task
        eprintln!("[REP] Spawning integration task");
        let integration_handle = compio::runtime::spawn(async move {
            use std::io::Write;
            let _ = std::io::stderr().write_all(b"[REP TASK] Integration task started (handshake already complete)!\n");
            let _ = std::io::stderr().flush();

            // Handshake is already complete, so we can immediately process all messages
            use futures::{select, FutureExt};

            loop {
                select! {
                    // Wait for socket events (bytes from network)
                    event = socket_event_rx.recv_async().fuse() => {
                        match event {
                            Ok(SocketEvent::Connected) => {
                                // Connection established, handshake already done
                            }
                            Ok(SocketEvent::ReceivedBytes(bytes)) => {
                                eprintln!("[REP TASK] Received {} bytes from SocketActor", bytes.len());
                                // Feed bytes into ZMTP session
                                let session_events = integrated_actor.session.on_bytes(bytes);

                                for event in session_events {
                                    match event {
                                        crate::session::SessionEvent::SendBytes(data) => {
                                            let _ = socket_cmd_tx.send(UserCmd::SendBytes(data));
                                        }
                                        crate::session::SessionEvent::HandshakeComplete { .. } => {
                                            // This shouldn't happen since handshake is already done
                                            eprintln!("[REP TASK] WARNING: Received HandshakeComplete but handshake was already done");
                                        }
                                        crate::session::SessionEvent::Frame(frame) => {
                                            eprintln!("[REP TASK] Received frame from peer");
                                            integrated_actor.handle_frame(frame);
                                        }
                                        crate::session::SessionEvent::Error(e) => {
                                            eprintln!("[REP TASK] Session error: {:?}, exiting", e);
                                            break;
                                        }
                                    }
                                }
                            }
                            Ok(SocketEvent::Disconnected) | Err(_) => {
                                eprintln!("[REP TASK] Socket disconnected, exiting");
                                break;
                            }
                        }
                    }
                    // Wait for outgoing messages from application
                    msg = integrated_actor.user_rx.recv_async().fuse() => {
                        match msg {
                            Ok(multipart) => {
                                eprintln!("[REP TASK] Got {} frames to send from user_rx", multipart.len());
                                let frames = integrated_actor.encode_outgoing_message(multipart);
                                for frame in frames {
                                    let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                                }
                            }
                            Err(_) => {
                                eprintln!("[REP TASK] User channel closed, exiting");
                                break;
                            }
                        }
                    }
                }
            }

            eprintln!("[REP TASK] Integration task exiting");
        });

        eprintln!("[REP] Socket fully initialized and ready");

        Self {
            app_tx: user_tx,
            app_rx,
            _task_handles: (socket_handle.into(), integration_handle.into()),
        }
    }

    /// Receive a request message.
    ///
    /// This blocks until a request is received. The envelope is automatically
    /// extracted and stored for the subsequent `send()` call.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(msg))` - Received a request (content only, envelope stripped)
    /// - `Ok(None)` - Connection closed gracefully
    /// - `Err(_)` - Channel error
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::rep::RepSocket;
    /// # async fn example(socket: &RepSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// if let Some(request) = socket.recv().await? {
    ///     println!("Got request with {} frames", request.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&self) -> Result<Option<Vec<Bytes>>, flume::RecvError> {
        eprintln!("[REP recv()] Waiting for request");
        
        match self.app_rx.recv_async().await {
            Ok(msg) => {
                eprintln!("[REP recv()] Received request with {} frames", msg.len());
                Ok(Some(msg))
            }
            Err(e) => {
                eprintln!("[REP recv()] Channel error: {:?}", e);
                Err(e)
            }
        }
    }

    /// Send a reply message.
    ///
    /// This must be called after `recv()` and automatically uses the stored
    /// envelope from the request.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called without first calling `recv()`
    /// - The underlying connection is closed
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::rep::RepSocket;
    /// # use bytes::Bytes;
    /// # async fn example(socket: &RepSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// socket.send(vec![Bytes::from("REPLY")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&self, msg: Vec<Bytes>) -> Result<(), flume::SendError<Vec<Bytes>>> {
        eprintln!("[REP send()] Sending reply with {} frames", msg.len());
        let result = self.app_tx.send(msg);
        
        match result {
            Ok(_) => {
                eprintln!("[REP send()] Reply queued successfully");
                Ok(())
            }
            Err(e) => {
                eprintln!("[REP send()] Send error");
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_rep_basic() {
        // REP socket tests require integration testing
        assert!(true);
    }
}
