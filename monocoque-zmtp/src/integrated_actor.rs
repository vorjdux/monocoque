//! ZMTP Integrated Actor
//!
//! This module provides the integration layer that composes:
//! - monocoque-core's protocol-agnostic SocketActor (IO primitive)
//! - monocoque-zmtp's ZmtpSession (protocol state machine)
//! - Hub connections (Router/PubSub)
//!
//! # Architecture
//!
//! ```text
//!     Application
//!          ↓
//!   ZmtpIntegratedActor  ← This layer (composition)
//!          ↓
//!   SocketActor (core) + ZmtpSession (protocol) + Hubs
//!          ↓
//!        IO
//! ```
//!
//! # Design Principles
//!
//! This layer follows the blueprint's strict separation of concerns:
//!
//! 1. **Protocol-agnostic core**: SocketActor knows nothing about ZMTP
//! 2. **Protocol layer**: ZmtpSession handles framing, handshake, commands
//! 3. **Integration layer**: ZmtpIntegratedActor composes them via events
//! 4. **No circular dependencies**: Core never imports protocol layers
//!
//! # Responsibilities
//!
//! - Forward raw bytes to ZmtpSession
//! - Assemble ZMTP frames into multipart messages
//! - Strip/inject ROUTER envelopes
//! - Parse SUB/UNSUB commands
//! - Register with appropriate hubs
//! - Convert hub commands back to ZMTP frames
//!
//! # Example
//!
//! ```rust,ignore
//! // Create integration actor
//! let actor = ZmtpIntegratedActor::new(
//!     session,
//!     SocketType::Router,
//!     Some(router_hub_tx),
//!     None,
//! );
//!
//! // Process bytes from SocketActor
//! let frames = actor.on_bytes(bytes);
//!
//! // Check for hub commands
//! let outgoing = actor.try_recv_peer_commands();
//! ```

use crate::codec::ZmtpFrame;
use crate::session::{SessionEvent, SocketType, ZmtpSession};

use bytes::Bytes;
use flume::{Receiver, Sender};
use std::sync::atomic::{AtomicU64, Ordering};

/// Global epoch counter for peer lifecycle tracking
static EPOCH_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Events from Router Hub to integrated actor
#[derive(Debug)]
pub enum HubEvent {
    PeerUp {
        routing_id: Bytes,
        tx: Sender<PeerCmd>,
    },
    PeerDown {
        routing_id: Bytes,
    },
}

/// Commands from Hub to peer
#[derive(Debug)]
pub enum PeerCmd {
    SendBody(Vec<Bytes>),
    Close,
}

/// Events for PubSub Hub
#[derive(Debug)]
pub enum PubSubEvent {
    PeerUp {
        routing_id: Bytes,
        epoch: u64,
        tx: Sender<PeerCmd>,
    },
    PeerDown {
        routing_id: Bytes,
        epoch: u64,
    },
    Subscribe {
        routing_id: Bytes,
        prefix: Bytes,
    },
    Unsubscribe {
        routing_id: Bytes,
        prefix: Bytes,
    },
}

/// Commands from Router Hub
#[derive(Debug)]
pub enum RouterCmd {
    SendMessage(Vec<Bytes>),
    Close,
}

/// Commands from PubSub Hub
#[derive(Debug)]
pub enum PubSubCmd {
    Publish(Vec<Bytes>),
    Close,
}

/// ZMTP-integrated socket actor that bridges core IO with protocol logic.
///
/// # Purpose
///
/// This is the **integration layer** that composes protocol-agnostic IO with
/// ZMTP protocol logic and hub coordination. It maintains strict architectural
/// boundaries per the blueprint:
///
/// - **Never** imports into monocoque-core (prevents circular dependencies)
/// - Composes core primitives via events (not inheritance)
/// - Handles protocol-specific logic (envelope stripping, command parsing)
///
/// # Responsibilities
///
/// 1. **Byte-to-Frame**: Forward bytes to ZmtpSession, receive frames
/// 2. **Multipart Assembly**: Collect frames into complete messages
/// 3. **Envelope Handling**: Strip ROUTER envelopes, inject on send
/// 4. **Command Parsing**: Decode SUB/UNSUB from payload
/// 5. **Hub Registration**: Connect to Router or PubSub hubs
/// 6. **Bidirectional Flow**: Hub commands → ZMTP frames
///
/// # Lifecycle
///
/// ```text
/// 1. Connected    → Register with hub
/// 2. ReceivedBytes → Parse frames → Assemble → Route to hub
/// 3. Hub command  → Encode frames → Send via SocketActor
/// 4. Disconnected → Unregister from hub
/// ```
///
/// # Thread Safety
///
/// Not Send/Sync - designed for single-threaded async context.
/// Use channels for cross-task communication.
pub struct ZmtpIntegratedActor {
    /// ZMTP protocol session state machine
    pub(crate) session: ZmtpSession,

    /// Socket type (determines routing behavior)
    pub(crate) socket_type: SocketType,

    /// Unique epoch for this connection
    epoch: u64,

    /// Peer's routing identity (set after handshake)
    routing_id: Option<Bytes>,

    /// Multipart message accumulator
    multipart: Vec<Bytes>,

    /// Channels for sending completed messages to application
    user_tx: Sender<Vec<Bytes>>,

    /// Channel for receiving messages from application
    pub(crate) user_rx: Receiver<Vec<Bytes>>,

    /// Optional Router Hub connection
    router_hub: Option<Sender<HubEvent>>,

    /// Optional PubSub Hub connection
    pubsub_hub: Option<Sender<PubSubEvent>>,

    /// Commands from hub to this peer
    peer_rx: Option<Receiver<PeerCmd>>,

    /// Sender for routing PeerCmd to the socket (for hub registration)
    peer_cmd_tx: Sender<PeerCmd>,

    /// Outbound frame queue (to be sent to SocketActor)
    write_queue: Vec<Bytes>,
}

impl ZmtpIntegratedActor {
    /// Create a new ZMTP integrated actor.
    pub fn new(
        socket_type: SocketType,
        user_tx: Sender<Vec<Bytes>>,
        user_rx: Receiver<Vec<Bytes>>,
    ) -> Self {
        // Use channel sender/receiver count as unique ID
        let tx_id = format!("{:p}-{}", &user_tx as *const _, user_tx.sender_count());
        let rx_id = format!("{:p}-{}", &user_rx as *const _, user_rx.receiver_count());
        eprintln!("[ZmtpIntegratedActor::new {:?}] TX ID: {}, RX ID: {}",
                 socket_type, tx_id, rx_id);
        let (peer_cmd_tx, peer_cmd_rx) = flume::unbounded();
        
        Self {
            session: ZmtpSession::new(socket_type),
            socket_type,
            epoch: EPOCH_COUNTER.fetch_add(1, Ordering::Relaxed),
            routing_id: None,
            multipart: Vec::new(),
            user_tx,
            user_rx,
            router_hub: None,
            pubsub_hub: None,
            peer_rx: Some(peer_cmd_rx),
            peer_cmd_tx,
            write_queue: Vec::new(),
        }
    }

    /// Main event loop for the integrated actor.
    ///
    /// This is the runtime-agnostic core that drives message flow:
    /// - User messages → ZMTP frames
    /// - Hub commands → ZMTP frames
    ///
    /// Returns frames to be sent via SocketActor.
    ///
    /// Call this method repeatedly in your async runtime.
    pub async fn process_events(&mut self) -> Vec<Bytes> {
        let mut outgoing = Vec::new();

        // Check channel status
        let queued = self.user_rx.len();
        let rx_id = std::ptr::addr_of!(self.user_rx) as usize;
        if queued > 0 {
            eprintln!("[INTEGRATED {:?}] user_rx ID={}, has {} items queued! Attempting to receive...", self.socket_type, rx_id, queued);
        } else {
            // Always print for ROUTER to see the ID even when empty
            if matches!(self.socket_type, crate::SocketType::Router) {
                eprintln!("[INTEGRATED {:?}] user_rx ID={}, queued={}", self.socket_type, rx_id, queued);
            }
        }

        // Check for user messages to send
        match self.user_rx.try_recv() {
            Ok(msg) => {
                eprintln!("[INTEGRATED {:?}] SUCCESS via try_recv! Got {} frames", 
                         self.socket_type, msg.len());
                let frames = self.encode_outgoing_message(msg);
                outgoing.extend(frames);
            }
            Err(flume::TryRecvError::Empty) => {
                // Try async receive as fallback
                if queued > 0 {
                    eprintln!("[INTEGRATED {:?}] try_recv returned Empty but {} items queued! Trying recv_async...", 
                             self.socket_type, queued);
                    match compio::time::timeout(std::time::Duration::from_micros(10), self.user_rx.recv_async()).await {
                        Ok(Ok(msg)) => {
                            eprintln!("[INTEGRATED {:?}] SUCCESS via recv_async! Got {} frames", 
                                     self.socket_type, msg.len());
                            let frames = self.encode_outgoing_message(msg);
                            outgoing.extend(frames);
                        }
                        Ok(Err(_)) => {
                            eprintln!("[INTEGRATED {:?}] recv_async: channel disconnected", self.socket_type);
                        }
                        Err(_) => {
                            eprintln!("[INTEGRATED {:?}] recv_async: timeout", self.socket_type);
                        }
                    }
                }
            }
            Err(flume::TryRecvError::Empty) => {
                // Silent - expected
            }
            Err(flume::TryRecvError::Disconnected) => {
                eprintln!("[INTEGRATED {:?}] WARNING: user_rx disconnected!", self.socket_type);
            }
        }

        // Check for hub commands
        let hub_frames = self.try_recv_peer_commands();
        outgoing.extend(hub_frames);

        outgoing
    }

    /// Encode outgoing multipart message into ZMTP frames.
    ///
    /// Handles DEALER and ROUTER semantics:
    /// - DEALER: multipart → frames with MORE flags
    /// - ROUTER: strip envelope, route body
    fn encode_outgoing_message(&mut self, parts: Vec<Bytes>) -> Vec<Bytes> {
        use crate::utils::encode_frame;

        if parts.is_empty() {
            return Vec::new();
        }

        let mut frames = Vec::new();

        match self.socket_type {
            SocketType::Dealer => {
                // DEALER: straightforward multipart
                let last_idx = parts.len() - 1;
                for (idx, part) in parts.into_iter().enumerate() {
                    let flags = if idx < last_idx { 0x01 } else { 0x00 };
                    frames.push(encode_frame(flags, &part));
                }
            }

            SocketType::Router => {
                // ROUTER expects: [RoutingID, Empty, Body...]
                if parts.len() >= 3 {
                    // Skip routing ID and delimiter, send body
                    let body = &parts[2..];
                    let last_idx = body.len() - 1;
                    for (idx, part) in body.iter().enumerate() {
                        let flags = if idx < last_idx { 0x01 } else { 0x00 };
                        frames.push(encode_frame(flags, part));
                    }
                }
            }

            SocketType::Pub => {
                // PUB: send as-is
                let last_idx = parts.len() - 1;
                for (idx, part) in parts.into_iter().enumerate() {
                    let flags = if idx < last_idx { 0x01 } else { 0x00 };
                    frames.push(encode_frame(flags, &part));
                }
            }

            _ => {
                // Default: send multipart
                let last_idx = parts.len() - 1;
                for (idx, part) in parts.into_iter().enumerate() {
                    let flags = if idx < last_idx { 0x01 } else { 0x00 };
                    frames.push(encode_frame(flags, &part));
                }
            }
        }

        frames
    }

    /// Attach Router hub for ROUTER/DEALER sockets.
    pub fn attach_router(&mut self, hub_tx: Sender<HubEvent>, peer_rx: Receiver<PeerCmd>) {
        self.router_hub = Some(hub_tx);
        self.peer_rx = Some(peer_rx);
    }

    /// Attach PubSub hub for PUB/SUB sockets.
    pub fn attach_pubsub(&mut self, hub_tx: Sender<PubSubEvent>, peer_rx: Receiver<PeerCmd>) {
        self.pubsub_hub = Some(hub_tx);
        self.peer_rx = Some(peer_rx);
    }

    /// Get the initial greeting to send on connection.
    pub fn local_greeting(&self) -> Bytes {
        self.session.local_greeting()
    }

    /// Process received bytes from the socket.
    ///
    /// Returns frames to be written back to the socket.
    pub fn on_bytes(&mut self, bytes: Bytes) -> Vec<Bytes> {
        let events = self.session.on_bytes(bytes);

        for event in events {
            match event {
                SessionEvent::HandshakeComplete {
                    peer_identity,
                    peer_socket_type: _,
                } => {
                    self.handle_handshake_complete(peer_identity);
                }

                SessionEvent::Frame(frame) => {
                    self.handle_frame(frame);
                }

                SessionEvent::SendBytes(b) => {
                    self.write_queue.push(b);
                }

                SessionEvent::Error(_) => {
                    // Connection should close
                    self.handle_disconnect();
                    break;
                }
            }
        }

        // Return any frames that need to be sent
        self.write_queue.drain(..).collect()
    }

    /// Process user messages (application → network).
    ///
    /// Returns encoded frames to send.
    pub fn on_user_message(&mut self, parts: Vec<Bytes>) -> Vec<Bytes> {
        let mut frames = Vec::new();
        let parts_len = parts.len();

        // ROUTER: strip envelope before framing
        let body_parts = if self.socket_type == SocketType::Router {
            // User sends [RoutingID, Empty, Body...]
            // We need to route to hub, not frame directly
            // This should go through hub routing instead
            parts // For now, pass through
        } else {
            parts
        };

        // Encode each part as a ZMTP frame
        for (i, part) in body_parts.into_iter().enumerate() {
            let more = i + 1 < parts_len;
            let frame = ZmtpFrame::data(part, more);
            frames.push(frame.encode());
        }

        frames
    }

    /// Process hub commands (Router/PubSub → network).
    ///
    /// Returns encoded frames to send.
    pub fn on_peer_command(&mut self, cmd: PeerCmd) -> Vec<Bytes> {
        match cmd {
            PeerCmd::SendBody(parts) => {
                let mut frames = Vec::new();
                let parts_len = parts.len();
                for (i, part) in parts.into_iter().enumerate() {
                    let more = i + 1 < parts_len;
                    let frame = ZmtpFrame::data(part, more);
                    frames.push(frame.encode());
                }
                frames
            }
            PeerCmd::Close => {
                self.handle_disconnect();
                Vec::new()
            }
        }
    }

    /// Check for pending peer commands (non-blocking).
    pub fn try_recv_peer_commands(&mut self) -> Vec<Bytes> {
        let mut frames = Vec::new();
        let mut commands = Vec::new();

        // Collect all pending commands first
        if let Some(peer_rx) = &self.peer_rx {
            while let Ok(cmd) = peer_rx.try_recv() {
                commands.push(cmd);
            }
        }

        // Process them (now peer_rx is no longer borrowed)
        for cmd in commands {
            frames.extend(self.on_peer_command(cmd));
        }

        frames
    }

    pub(crate) fn handle_handshake_complete(&mut self, peer_identity: Option<Bytes>) {
        let rid = peer_identity
            .unwrap_or_else(|| Bytes::from(format!("anon-{}", self.epoch)));

        self.routing_id = Some(rid.clone());

        // Notify Router hub
        if let Some(hub) = &self.router_hub {
            let _ = hub.send(HubEvent::PeerUp {
                routing_id: rid.clone(),
                tx: self.peer_cmd_tx.clone(),
            });
        }

        // Notify PubSub hub
        if let Some(hub) = &self.pubsub_hub {
            let _ = hub.send(PubSubEvent::PeerUp {
                routing_id: rid.clone(),
                epoch: self.epoch,
                tx: self.peer_cmd_tx.clone(),
            });
        }
    }

    pub(crate) fn handle_frame(&mut self, frame: ZmtpFrame) {
        eprintln!("[INTEGRATED {:?}] handle_frame called, is_command={}, has_more={}", 
                  self.socket_type, frame.is_command(), frame.more());
        
        // Handle commands (SUB/UNSUB)
        if frame.is_command() {
            self.handle_command(frame);
            return;
        }

        // Check if MORE flag is set before consuming
        let has_more = frame.more();
        
        eprintln!("[INTEGRATED {:?}] Accumulating frame, current multipart count: {}", 
                  self.socket_type, self.multipart.len());
        
        // Accumulate multipart message
        self.multipart.push(frame.payload);

        // Message complete?
        if !has_more {
            let mut msg = std::mem::take(&mut self.multipart);

            // ROUTER: inject envelope
            if self.socket_type == SocketType::Router {
                if let Some(rid) = &self.routing_id {
                    msg.insert(0, Bytes::new()); // Empty delimiter
                    msg.insert(0, rid.clone());  // Routing ID
                }
            }

            // DEBUG: Log received message
            eprintln!("[INTEGRATED {:?}] Received message with {} frames, sending to app", 
                     self.socket_type, msg.len());

            // Send to application
            match self.user_tx.send(msg) {
                Ok(_) => eprintln!("[INTEGRATED {:?}] Successfully sent message to app", self.socket_type),
                Err(e) => eprintln!("[INTEGRATED {:?}] Failed to send message to app: {:?}", self.socket_type, e),
            }
        }
    }

    fn handle_command(&mut self, frame: ZmtpFrame) {
        // Parse command name
        let payload = frame.payload.as_ref();
        
        eprintln!("[INTEGRATED {:?}] Received command, payload len={}, first bytes={:?}", 
                  self.socket_type, payload.len(), &payload[..payload.len().min(20)]);
        
        if payload.is_empty() {
            return;
        }

        // Commands are typically: <name_len><name><data>
        // For now, simple parsing for SUB/UNSUB
        if payload.starts_with(&[3]) && &payload[1..4] == b"SUB" {
            // SUB command
            let prefix = if payload.len() > 4 {
                Bytes::copy_from_slice(&payload[4..])
            } else {
                Bytes::new() // Empty prefix = subscribe to all
            };

            if let (Some(rid), Some(hub)) = (&self.routing_id, &self.pubsub_hub) {
                let _ = hub.send(PubSubEvent::Subscribe {
                    routing_id: rid.clone(),
                    prefix,
                });
            }
        } else if payload.starts_with(&[5]) && &payload[1..6] == b"UNSUB" {
            // UNSUB command  
            let prefix = if payload.len() > 6 {
                Bytes::copy_from_slice(&payload[6..])
            } else {
                Bytes::new()
            };

            if let (Some(rid), Some(hub)) = (&self.routing_id, &self.pubsub_hub) {
                let _ = hub.send(PubSubEvent::Unsubscribe {
                    routing_id: rid.clone(),
                    prefix,
                });
            }
        }
    }

    fn handle_disconnect(&mut self) {
        if let Some(rid) = &self.routing_id {
            // Notify Router hub
            if let Some(hub) = &self.router_hub {
                let _ = hub.send(HubEvent::PeerDown {
                    routing_id: rid.clone(),
                });
            }

            // Notify PubSub hub
            if let Some(hub) = &self.pubsub_hub {
                let _ = hub.send(PubSubEvent::PeerDown {
                    routing_id: rid.clone(),
                    epoch: self.epoch,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_actor_with_epoch() {
        let (tx, _rx) = flume::unbounded();
        let (_user_tx, user_rx) = flume::unbounded();
        
        let actor = ZmtpIntegratedActor::new(SocketType::Dealer, tx, user_rx);
        assert!(actor.epoch > 0);
        assert!(actor.routing_id.is_none());
    }

    #[test]
    fn assembles_multipart_messages() {
        let (tx, rx) = flume::unbounded();
        let (_user_tx, user_rx) = flume::unbounded();
        
        let mut actor = ZmtpIntegratedActor::new(SocketType::Dealer, tx, user_rx);
        
        // Simulate frames
        let frame1 = ZmtpFrame {
            flags: 0x01, // MORE
            payload: Bytes::from_static(b"part1"),
        };
        let frame2 = ZmtpFrame {
            flags: 0x00, // No MORE
            payload: Bytes::from_static(b"part2"),
        };

        actor.handle_frame(frame1);
        assert!(rx.try_recv().is_err()); // Not complete yet

        actor.handle_frame(frame2);
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.len(), 2);
        assert_eq!(msg[0].as_ref(), b"part1");
        assert_eq!(msg[1].as_ref(), b"part2");
    }
}
