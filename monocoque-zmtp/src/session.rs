use crate::codec::{ZmtpDecoder, ZmtpError, ZmtpFrame};
use crate::greeting::ZmtpGreeting;
use bytes::{Bytes, BytesMut};
use monocoque_core::buffer::SegmentedBuffer;

/// Supported ZMQ socket types (no heap allocation)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketType {
    Pair,
    Dealer,
    Router,
    Pub,
    Sub,
    Req,
    Rep,
    Push,
    Pull,
}

impl SocketType {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Pair => "PAIR",
            Self::Dealer => "DEALER",
            Self::Router => "ROUTER",
            Self::Pub => "PUB",
            Self::Sub => "SUB",
            Self::Req => "REQ",
            Self::Rep => "REP",
            Self::Push => "PUSH",
            Self::Pull => "PULL",
        }
    }
}

/// Events emitted by the session (transport-agnostic)
pub enum SessionEvent {
    /// Send raw bytes immediately (greeting / handshake)
    SendBytes(Bytes),

    /// A validated ZMTP frame
    Frame(ZmtpFrame),

    /// Handshake completed successfully
    HandshakeComplete {
        peer_identity: Option<Bytes>,
        peer_socket_type: SocketType,
    },

    /// Fatal protocol error
    Error(ZmtpError),
}

enum State {
    Greeting {
        buffer: BytesMut,
    },
    Handshake {
        decoder: ZmtpDecoder,
        peer_socket_type: Option<SocketType>,
        peer_identity: Option<Bytes>,
    },
    Active {
        decoder: ZmtpDecoder,
    },
}

/// Sans-IO ZMTP session
pub struct ZmtpSession {
    state: State,
    local_socket_type: SocketType,
    recv: SegmentedBuffer,
}

impl ZmtpSession {
    #[must_use]
    pub fn new(local_socket_type: SocketType) -> Self {
        Self {
            state: State::Greeting {
                buffer: BytesMut::with_capacity(64),
            },
            local_socket_type,
            recv: SegmentedBuffer::new(),
        }
    }

    /// Create a session that's already past the handshake phase.
    ///
    /// Use this when handshake has been performed synchronously before
    /// spawning the session actor.
    #[must_use]
    pub fn new_active(local_socket_type: SocketType) -> Self {
        Self {
            state: State::Active {
                decoder: ZmtpDecoder::new(),
            },
            local_socket_type,
            recv: SegmentedBuffer::new(),
        }
    }

    /// Generate our greeting bytes
    ///
    /// # Compatibility
    ///
    /// Sends ZMTP 3.0 greeting for maximum backward compatibility with `ZeroMQ` 4.1+.
    /// The implementation accepts any ZMTP 3.x version from peers, ensuring
    /// compatibility with all modern ZMQ versions (4.1, 4.2, 4.3, 4.4).
    pub fn local_greeting(&self) -> Bytes {
        let mut b = BytesMut::with_capacity(64);

        // Signature
        b.extend_from_slice(&[0xFF]);
        b.extend_from_slice(&[0u8; 8]);
        b.extend_from_slice(&[0x7F]);

        // Version 3.0 (backward compatible with all ZMQ 4.x)
        b.extend_from_slice(&[0x03, 0x00]);

        // Mechanism: NULL (Phase 1–3)
        b.extend_from_slice(b"NULL");
        b.extend_from_slice(&[0u8; 16]);

        // As-server flag = 0 for NULL
        b.extend_from_slice(&[0x00]);

        // Padding
        b.extend_from_slice(&[0u8; 31]);

        b.freeze()
    }

    /// Feed incoming bytes into the session
    pub fn on_bytes(&mut self, src: Bytes) -> Vec<SessionEvent> {
        let mut events = Vec::new();

        self.recv.push(src);

        loop {
            match &mut self.state {
                // =========================
                // Greeting
                // =========================
                State::Greeting { buffer } => {
                    let needed = 64 - buffer.len();
                    let take = needed.min(self.recv.len());
                    if let Some(bytes) = self.recv.take_bytes(take) {
                        buffer.extend_from_slice(&bytes);
                    }

                    if buffer.len() < 64 {
                        break;
                    }

                    let greeting = buffer.split().freeze();

                    match ZmtpGreeting::parse(&greeting) {
                        Ok(_g) => {
                            // Transition to handshake
                            self.state = State::Handshake {
                                decoder: ZmtpDecoder::new(),
                                peer_socket_type: None,
                                peer_identity: None,
                            };

                            // Send our greeting (if we haven't already)
                            // Note: In connect scenario, greeting is sent first by us
                            // In accept scenario, we send after receiving theirs
                            // events.push(SessionEvent::SendBytes(self.local_greeting()));

                            // Send READY command immediately after greeting exchange
                            use crate::utils::{build_ready, encode_frame, FLAG_COMMAND};
                            let socket_type_str = match self.local_socket_type {
                                SocketType::Dealer => "DEALER",
                                SocketType::Router => "ROUTER",
                                SocketType::Pub => "PUB",
                                SocketType::Sub => "SUB",
                                _ => "DEALER",
                            };
                            let ready_body = build_ready(socket_type_str, None);
                            let ready_frame = encode_frame(FLAG_COMMAND, &ready_body);
                            events.push(SessionEvent::SendBytes(ready_frame));
                        }
                        Err(e) => {
                            events.push(SessionEvent::Error(e));
                            break;
                        }
                    }
                }

                // =========================
                // Handshake
                // =========================
                State::Handshake {
                    decoder,
                    peer_socket_type,
                    peer_identity,
                } => {
                    match decoder.decode(&mut self.recv) {
                        Ok(Some(frame)) => {
                            if !frame.is_command() {
                                events.push(SessionEvent::Error(ZmtpError::Protocol));
                                break;
                            }

                            // Extract values before transitioning state
                            let peer_id = peer_identity.take();
                            let peer_st = peer_socket_type.unwrap_or(self.local_socket_type);

                            // Create new decoder for Active state
                            let new_decoder = ZmtpDecoder::new();
                            let old_decoder = std::mem::replace(decoder, new_decoder);

                            // Now transition state
                            self.state = State::Active {
                                decoder: old_decoder,
                            };

                            events.push(SessionEvent::HandshakeComplete {
                                peer_identity: peer_id,
                                peer_socket_type: peer_st,
                            });
                        }
                        Ok(None) => break,
                        Err(e) => {
                            events.push(SessionEvent::Error(e));
                            break;
                        }
                    }
                }

                // =========================
                // Active
                // =========================
                State::Active { decoder } => match decoder.decode(&mut self.recv) {
                    Ok(Some(frame)) => {
                        events.push(SessionEvent::Frame(frame));
                    }
                    Ok(None) => break,
                    Err(e) => {
                        events.push(SessionEvent::Error(e));
                        break;
                    }
                },
            }
        }

        events
    }
}
