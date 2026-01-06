//! # DEALER Socket Implementation
//!
//! The DEALER socket provides asynchronous request-reply patterns with load balancing.
//!
//! ## Features
//!
//! - **Bidirectional**: Can both send and receive multipart messages
//! - **Load Balanced**: When multiple DEALER sockets connect to a ROUTER, messages are distributed fairly
//! - **Asynchronous**: Non-blocking send and receive operations
//! - **Multipart**: Full support for ZeroMQ multipart messages
//!
//! ## Usage Example
//!
//! ```rust,no_run
//! use monocoque_zmtp::dealer::DealerSocket;
//! use compio::net::TcpStream;
//! use bytes::Bytes;
//!
//! #[compio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to ROUTER server
//!     let stream = TcpStream::connect("127.0.0.1:5555").await?;
//!     let socket = DealerSocket::new(stream).await;
//!     
//!     // Send request
//!     socket.send(vec![Bytes::from("Hello")]).await?;
//!     
//!     // Receive response
//!     let response = socket.recv().await?;
//!     println!("Got {} frames", response.len());
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Protocol Details
//!
//! DEALER implements the ZeroMQ DEALER socket pattern:
//! - Messages are sent as-is (no envelope modification)
//! - Compatible with ROUTER and REP sockets
//! - Fair queuing when multiple DEALERs connect to one ROUTER

use crate::{integrated_actor::ZmtpIntegratedActor, session::SocketType};
use bytes::Bytes;
use compio::net::TcpStream;
use flume::{unbounded, Receiver, Sender};
use monocoque_core::{
    actor::{SocketActor, SocketEvent, UserCmd},
    alloc::IoArena,
};

/// A DEALER socket for asynchronous request-reply patterns.
///
/// DEALER sockets provide:
/// - Bidirectional communication (send and receive)
/// - Multipart message support
/// - Load balancing when connecting to ROUTER sockets
/// - Asynchronous, non-blocking operations
///
/// # Architecture
///
/// The socket integrates three layers:
/// 1. `SocketActor` - Protocol-agnostic I/O with split read/write pumps
/// 2. `ZmtpIntegratedActor` - ZMTP protocol handling (framing, handshake)
/// 3. Application API - High-level async send/recv
///
/// # Example
///
/// ```rust,no_run
/// use monocoque_zmtp::dealer::DealerSocket;
/// use compio::net::TcpStream;
/// use bytes::Bytes;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let stream = TcpStream::connect("127.0.0.1:5555").await?;
/// let socket = DealerSocket::new(stream).await;
///
/// // Send a request
/// socket.send(vec![Bytes::from("REQUEST")]).await?;
///
/// // Receive response
/// let reply = socket.recv().await?;
/// # Ok(())
/// # }
/// ```
pub struct DealerSocket {
    /// Channel to send messages to application
    app_tx: Sender<Vec<Bytes>>,
    /// Channel to receive messages from application
    app_rx: Receiver<Vec<Bytes>>,
    /// Task handles (kept alive to prevent task cancellation)
    _task_handles: (compio::runtime::Task<()>, compio::runtime::Task<()>),
}

impl DealerSocket {
    /// Create a new DEALER socket from an established TCP stream.
    ///
    /// **Internal API**: For public-facing ergonomics, use `monocoque::DealerSocket::connect()`.
    ///
    /// This spawns background tasks for I/O, protocol handling, and routing.
    pub async fn new(stream: TcpStream) -> Self {
        // Create channels
        let (socket_event_tx, socket_event_rx) = unbounded(); // SocketActor → integration
        let (socket_cmd_tx, socket_cmd_rx) = unbounded(); // integration → SocketActor
        let (app_tx, app_rx) = unbounded(); // integrated → application (for recv)
        let (user_tx, user_rx) = unbounded(); // application → integrated (for send)

        // Create SocketActor
        let arena = IoArena::new();
        let socket_actor = SocketActor::new(stream, socket_event_tx, socket_cmd_rx, arena);

        // Create ZmtpIntegratedActor
        let mut integrated_actor = ZmtpIntegratedActor::new(
            SocketType::Dealer,
            app_tx,  // integrated sends received messages TO app
            user_rx, // integrated receives outgoing messages FROM app
        );

        // Send initial greeting
        let greeting = integrated_actor.local_greeting();
        let _ = socket_cmd_tx.send(UserCmd::SendBytes(greeting));

        // Spawn the integration task
        let socket_cmd_tx_clone = socket_cmd_tx.clone();
        eprintln!("[DEALER] About to spawn integration task");
        let integration_handle = compio::runtime::spawn(async move {
            use std::io::Write;
            let _ = std::io::stderr().write_all(b"[DEALER TASK] Integration task started!\n");
            let _ = std::io::stderr().flush();
            // This task bridges SocketActor events to ZmtpIntegratedActor
            let mut handshake_complete = false;

            loop {
                // Process socket events
                if let Ok(event) = socket_event_rx.try_recv() {
                    match event {
                        SocketEvent::Connected => {
                            // Connection established, greeting already sent
                        }
                        SocketEvent::ReceivedBytes(bytes) => {
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
                                        // READY command already sent by Session
                                    }
                                    crate::session::SessionEvent::Frame(frame) => {
                                        if handshake_complete {
                                            eprintln!("[DEALER TASK] Received frame from peer, passing to integrated_actor");
                                            integrated_actor.handle_frame(frame);
                                        }
                                    }
                                    crate::session::SessionEvent::Error(_e) => {
                                        eprintln!("[DEALER TASK] Session error, exiting");
                                        break;
                                    }
                                }
                            }
                        }
                        SocketEvent::Disconnected => {
                            eprintln!("[DEALER TASK] Socket disconnected, exiting");
                            break;
                        }
                    }
                }

                // Process outgoing messages
                let outgoing_frames = integrated_actor.process_events().await;
                if !outgoing_frames.is_empty() {
                    eprintln!(
                        "[DEALER TASK] Got {} outgoing frames from process_events()",
                        outgoing_frames.len()
                    );
                }
                for frame in outgoing_frames {
                    eprintln!("[DEALER TASK] Sending {} bytes to SocketActor", frame.len());
                    let _ = socket_cmd_tx.send(UserCmd::SendBytes(frame));
                }

                // Small yield to prevent busy-waiting
                compio::time::sleep(std::time::Duration::from_micros(100)).await;
            }

            eprintln!("[DEALER TASK] Integration task exited!");
        });

        // Spawn SocketActor
        eprintln!("[DEALER] About to spawn SocketActor");
        let socket_handle = compio::runtime::spawn(socket_actor.run());
        eprintln!("[DEALER] SocketActor spawned, returning socket");

        // Yield to allow spawned tasks to start
        compio::time::sleep(std::time::Duration::from_micros(1)).await;

        DealerSocket {
            app_tx: user_tx,
            app_rx,
            _task_handles: (integration_handle, socket_handle),
        }
    }

    /// Send a multipart message asynchronously.
    ///
    /// # Arguments
    ///
    /// * `parts` - Vector of message frames to send
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use monocoque_zmtp::dealer::DealerSocket;
    /// # use bytes::Bytes;
    /// # async fn example(socket: &DealerSocket) {
    /// socket.send(vec![Bytes::from("Hello")]).await.unwrap();
    /// # }
    /// ```
    pub async fn send(&self, parts: Vec<Bytes>) -> Result<(), flume::SendError<Vec<Bytes>>> {
        eprintln!(
            "[DEALER send()] Sending {} frames to channel {:?}",
            parts.len(),
            parts
                .iter()
                .map(|p| String::from_utf8_lossy(p).to_string())
                .collect::<Vec<_>>()
        );
        let result = self.app_tx.send_async(parts).await;
        eprintln!(
            "[DEALER send()] Channel send result: {:?}",
            result.as_ref().map(|_| "OK")
        );
        result
    }

    /// Receive a multipart message asynchronously.
    ///
    /// # Returns
    ///
    /// A vector of message frames.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use monocoque_zmtp::dealer::DealerSocket;
    /// # async fn example(socket: &DealerSocket) {
    /// let message = socket.recv().await.unwrap();
    /// println!("Got {} frames", message.len());
    /// # }
    /// ```
    pub async fn recv(&self) -> Result<Vec<Bytes>, flume::RecvError> {
        eprintln!("[DEALER recv()] Waiting for message from channel");
        let result = self.app_rx.recv_async().await;
        if let Ok(ref msg) = result {
            eprintln!("[DEALER recv()] Received {} frames from channel", msg.len());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dealer_creation() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            // This test validates the structure compiles
            // Real testing requires a connected stream
            println!("DEALER socket structure validated");
        });
    }
}
