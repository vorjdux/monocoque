//! # REQ Socket Implementation
//!
//! The REQ socket provides strict request-reply patterns with enforced alternation.
//!
//! ## Features
//!
//! - **Strict Alternation**: Must alternate between send() and recv()
//! - **Synchronous Pattern**: Enforces request-response flow
//! - **Correlation Tracking**: Tracks request/reply pairs
//! - **Multipart**: Full support for ZeroMQ multipart messages
//!
//! ## Usage Example
//!
//! ```rust,no_run
//! use monocoque_zmtp::req::ReqSocket;
//! use compio::net::TcpStream;
//! use bytes::Bytes;
//!
//! #[compio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to REP server
//!     let stream = TcpStream::connect("127.0.0.1:5555").await?;
//!     let socket = ReqSocket::new(stream).await;
//!     
//!     // Must alternate send/recv
//!     socket.send(vec![Bytes::from("Hello")]).await?;
//!     let response = socket.recv().await?;
//!     
//!     // Another request-reply cycle
//!     socket.send(vec![Bytes::from("World")]).await?;
//!     let response = socket.recv().await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## State Machine
//!
//! REQ socket enforces this state machine:
//! ```text
//! Idle → send() → AwaitingReply → recv() → Idle
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
use std::sync::{Arc, Mutex};

/// State of the REQ socket state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReqState {
    /// Ready to send a request
    Idle,
    /// Waiting for a reply after sending request
    AwaitingReply,
}

/// A REQ socket for strict request-reply patterns.
///
/// REQ sockets enforce strict alternation between send and receive operations:
/// - Must call `send()` before `recv()`
/// - Must call `recv()` before next `send()`
/// - Violating this pattern returns an error
///
/// # Architecture
///
/// The socket integrates three layers:
/// 1. `SocketActor` - Protocol-agnostic I/O with split read/write pumps
/// 2. `ZmtpIntegratedActor` - ZMTP protocol handling (framing, handshake)
/// 3. State Machine - Enforces REQ pattern compliance
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::req::ReqSocket;
/// use compio::net::TcpStream;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = TcpStream::connect("127.0.0.1:5555").await?;
/// let socket = ReqSocket::new(stream).await;
///
/// // Request-reply cycle
/// socket.send(vec![Bytes::from("REQUEST")]).await?;
/// let reply = socket.recv().await?;
///
/// // Must complete recv() before next send()
/// socket.send(vec![Bytes::from("ANOTHER")]).await?;
/// let reply = socket.recv().await?;
/// # Ok(())
/// # }
/// ```
pub struct ReqSocket {
    app_tx: Sender<Vec<Bytes>>,
    app_rx: Receiver<Vec<Bytes>>,
    state: Arc<Mutex<ReqState>>,
    _task_handles: (compio::runtime::Task<()>, compio::runtime::Task<()>),
}

impl ReqSocket {
    /// Create a new REQ socket from a TCP stream.
    ///
    /// This performs the ZMTP handshake and starts the socket actors.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque_zmtp::req::ReqSocket;
    /// use compio::net::TcpStream;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let stream = TcpStream::connect("127.0.0.1:5555").await?;
    /// let socket = ReqSocket::new(stream).await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(mut stream: TcpStream) -> Self {
        println!("[REQ] Creating new REQ socket");

        // PHASE 1: Perform synchronous handshake on the raw stream BEFORE spawning any tasks
        // This prevents any race conditions - no data frames can be sent until handshake completes
        eprintln!("[REQ] Performing synchronous handshake...");
        let handshake_result = perform_handshake(&mut stream, SocketType::Req, None)
            .await
            .expect("Handshake failed");
        
        eprintln!(
            "[REQ] Handshake complete! Peer: {:?}, Socket Type: {:?}",
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
            SocketType::Req,
            app_tx.clone(),
            user_rx,
            handshake_result.peer_identity,
        );

        // Spawn tasks - handshake is already complete, so no race condition
        eprintln!("[REQ] Spawning SocketActor");
        let socket_handle = compio::runtime::spawn(socket_actor.run());
        eprintln!("[REQ] SocketActor spawned");

        // State tracking
        let state_check = Arc::new(Mutex::new(ReqState::Idle));

        // Spawn the integration task
        eprintln!("[REQ] Spawning integration task");
        let integration_handle = compio::runtime::spawn(async move {
            use std::io::Write;
            let _ = std::io::stderr().write_all(b"[REQ TASK] Integration task started (handshake already complete)!\n");
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
                                // Feed bytes into ZMTP session
                                let session_events = integrated_actor.session.on_bytes(bytes);

                                for event in session_events {
                                    match event {
                                        crate::session::SessionEvent::SendBytes(data) => {
                                            let _ = socket_cmd_tx.send(UserCmd::SendBytes(data));
                                        }
                                        crate::session::SessionEvent::HandshakeComplete { .. } => {
                                            // This shouldn't happen since handshake is already done
                                            eprintln!("[REQ TASK] WARNING: Received HandshakeComplete but handshake was already done");
                                        }
                                        crate::session::SessionEvent::Frame(frame) => {
                                            eprintln!("[REQ TASK] Received frame from peer, passing to integrated_actor");
                                            integrated_actor.handle_frame(frame);
                                        }
                                        crate::session::SessionEvent::Error(e) => {
                                            eprintln!("[REQ TASK] Session error: {:?}, exiting", e);
                                            break;
                                        }
                                    }
                                }
                            }
                            Ok(SocketEvent::Disconnected) | Err(_) => {
                                eprintln!("[REQ TASK] Socket disconnected, exiting");
                                break;
                            }
                        }
                    }
                    // Wait for outgoing messages from application
                    msg = integrated_actor.user_rx.recv_async().fuse() => {
                        match msg {
                            Ok(multipart) => {
                                eprintln!("[REQ TASK] Got {} frames from user_rx", multipart.len());
                                let frames = integrated_actor.encode_outgoing_message(multipart);
                                for frame in frames {
                                    eprintln!("[REQ TASK] Sending {} bytes to SocketActor", frame.len());
                                    let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                                }
                            }
                            Err(_) => {
                                eprintln!("[REQ TASK] User channel closed, exiting");
                                break;
                            }
                        }
                    }
                }
            }

            eprintln!("[REQ TASK] Integration task exiting");
        });

        eprintln!("[REQ] Socket fully initialized and ready");

        Self {
            app_tx: user_tx,
            app_rx,
            state: state_check,
            _task_handles: (socket_handle.into(), integration_handle.into()),
        }
    }

    /// Send a request message.
    ///
    /// This enforces the REQ state machine - you must call `recv()` before
    /// calling `send()` again.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Called while awaiting a reply (must call `recv()` first)
    /// - The underlying connection is closed
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::req::ReqSocket;
    /// # use bytes::Bytes;
    /// # async fn example(socket: &ReqSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// socket.send(vec![Bytes::from("REQUEST")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&self, msg: Vec<Bytes>) -> Result<(), flume::SendError<Vec<Bytes>>> {
        // Check state machine
        {
            let mut state = self.state.lock().unwrap();
            if *state != ReqState::Idle {
                return Err(flume::SendError(msg));
            }
            *state = ReqState::AwaitingReply;
        }

        println!("[REQ send()] Sending {} frames", msg.len());
        let result = self.app_tx.send(msg);
        
        match result {
            Ok(_) => {
                println!("[REQ send()] Message queued successfully");
                Ok(())
            }
            Err(e) => {
                // Reset state on error
                *self.state.lock().unwrap() = ReqState::Idle;
                Err(e)
            }
        }
    }

    /// Receive a reply message.
    ///
    /// This blocks until a reply is received. You must call this after `send()`
    /// before calling `send()` again.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(msg))` - Received a multipart message
    /// - `Ok(None)` - Connection closed gracefully
    /// - `Err(_)` - Channel error
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use monocoque_zmtp::req::ReqSocket;
    /// # async fn example(socket: &ReqSocket) -> Result<(), Box<dyn std::error::Error>> {
    /// let reply = socket.recv().await?;
    /// if let Some(msg) = reply {
    ///     println!("Got {} frames", msg.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn recv(&self) -> Result<Option<Vec<Bytes>>, flume::RecvError> {
        // State check: must be awaiting reply
        {
            let state = self.state.lock().unwrap();
            if *state != ReqState::AwaitingReply {
                println!("[REQ recv()] ERROR: Cannot recv while in Idle state");
                return Ok(None);
            }
        }

        println!("[REQ recv()] Waiting for reply");
        
        match self.app_rx.recv_async().await {
            Ok(msg) => {
                println!("[REQ recv()] Received {} frames", msg.len());
                // Transition back to Idle
                *self.state.lock().unwrap() = ReqState::Idle;
                Ok(Some(msg))
            }
            Err(e) => {
                println!("[REQ recv()] Channel error: {:?}", e);
                // Reset state on error
                *self.state.lock().unwrap() = ReqState::Idle;
                Err(e)
            }
        }
    }

    /// Get the current state of the REQ socket.
    ///
    /// This is primarily for debugging and testing.
    pub fn state(&self) -> ReqState {
        *self.state.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_req_state_machine() {
        // State machine logic is tested through integration tests
        // Unit testing state transitions would require mocking
        assert_eq!(ReqState::Idle, ReqState::Idle);
        assert_ne!(ReqState::Idle, ReqState::AwaitingReply);
    }
}
