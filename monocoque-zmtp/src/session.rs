use crate::codec::{ZmtpDecoder, ZmtpError, ZmtpFrame};
use crate::greeting::ZmtpGreeting;
use crate::handshake::parse_ready_command;
use bytes::{Bytes, BytesMut};
use monocoque_core::buffer::SegmentedBuffer;

/// Supported ZMQ socket types (no heap allocation)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketType {
    /// PAIR socket type.
    Pair,
    /// DEALER socket type.
    Dealer,
    /// ROUTER socket type.
    Router,
    /// PUB socket type.
    Pub,
    /// SUB socket type.
    Sub,
    /// REQ socket type.
    Req,
    /// REP socket type.
    Rep,
    /// PUSH socket type.
    Push,
    /// PULL socket type.
    Pull,
    /// XPUB socket type.
    Xpub,
    /// XSUB socket type.
    Xsub,
}

impl SocketType {
    /// Return the wire-format name string for this socket type.
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
            Self::Xpub => "XPUB",
            Self::Xsub => "XSUB",
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
        /// Peer's ZMQ identity, if provided.
        peer_identity: Option<Bytes>,
        /// Socket type advertised by the peer.
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
    /// Resolved `max_msg_size` applied to every decoder this session creates.
    /// `None` keeps the decoder's built-in default cap.
    max_frame_size: Option<usize>,
}

/// Build a decoder honoring an optional `max_msg_size` limit.
fn make_decoder(max_frame_size: Option<usize>) -> ZmtpDecoder {
    max_frame_size.map_or_else(ZmtpDecoder::new, ZmtpDecoder::with_max_frame_size)
}

impl ZmtpSession {
    /// Create a new ZMTP session starting in the greeting phase.
    ///
    /// Uses the decoder's built-in default frame-size cap. Use
    /// [`Self::with_max_frame_size`] to enforce a custom `max_msg_size`.
    #[must_use]
    pub fn new(local_socket_type: SocketType) -> Self {
        Self::with_max_frame_size(local_socket_type, None)
    }

    /// Create a session in the greeting phase that rejects frames whose declared
    /// body length exceeds `max_frame_size` (the socket's `max_msg_size`).
    ///
    /// `None` keeps the decoder's built-in default cap.
    #[must_use]
    pub fn with_max_frame_size(
        local_socket_type: SocketType,
        max_frame_size: Option<usize>,
    ) -> Self {
        Self {
            state: State::Greeting {
                buffer: BytesMut::with_capacity(64),
            },
            local_socket_type,
            recv: SegmentedBuffer::new(),
            max_frame_size,
        }
    }

    /// Create a session that's already past the handshake phase.
    ///
    /// Use this when handshake has been performed synchronously before
    /// spawning the session actor.
    #[must_use]
    pub fn new_active(local_socket_type: SocketType) -> Self {
        Self::new_active_with_max_frame_size(local_socket_type, None)
    }

    /// Create an already-active session that enforces `max_frame_size`
    /// (the socket's `max_msg_size`) on its decoder.
    ///
    /// `None` keeps the decoder's built-in default cap.
    #[must_use]
    pub fn new_active_with_max_frame_size(
        local_socket_type: SocketType,
        max_frame_size: Option<usize>,
    ) -> Self {
        Self {
            state: State::Active {
                decoder: make_decoder(max_frame_size),
            },
            local_socket_type,
            recv: SegmentedBuffer::new(),
            max_frame_size,
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

        // Mechanism: NULL (Phase 1-3)
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
                                decoder: make_decoder(self.max_frame_size),
                                peer_socket_type: None,
                                peer_identity: None,
                            };

                            // Send our greeting (if we haven't already)
                            // Note: In connect scenario, greeting is sent first by us
                            // In accept scenario, we send after receiving theirs
                            // events.push(SessionEvent::SendBytes(self.local_greeting()));

                            // Send READY command immediately after greeting exchange
                            use crate::utils::{FLAG_COMMAND, build_ready, encode_frame};
                            let socket_type_str = match self.local_socket_type {
                                SocketType::Dealer => "DEALER",
                                SocketType::Router => "ROUTER",
                                SocketType::Pub => "PUB",
                                SocketType::Sub => "SUB",
                                SocketType::Xpub => "XPUB",
                                SocketType::Xsub => "XSUB",
                                SocketType::Req => "REQ",
                                SocketType::Rep => "REP",
                                SocketType::Push => "PUSH",
                                SocketType::Pull => "PULL",
                                SocketType::Pair => "PAIR",
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

                            let (parsed_socket_type, parsed_identity) =
                                match parse_ready_command(&frame.payload) {
                                    Ok(parsed) => parsed,
                                    Err(e) => {
                                        events.push(SessionEvent::Error(e));
                                        break;
                                    }
                                };
                            *peer_socket_type = Some(parsed_socket_type);
                            *peer_identity = parsed_identity;

                            // Extract values before transitioning state
                            let peer_id = peer_identity.take();
                            let peer_st = peer_socket_type.unwrap_or(self.local_socket_type);

                            // Reuse the handshake decoder for the Active state; the
                            // replacement is a throwaway needed only for mem::replace.
                            let new_decoder = make_decoder(self.max_frame_size);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{FLAG_COMMAND, build_ready, encode_frame};

    /// An active session created with a `max_msg_size` rejects a frame whose
    /// declared body length exceeds the limit, before reading the body.
    #[test]
    fn active_session_enforces_max_frame_size() {
        let mut session = ZmtpSession::new_active_with_max_frame_size(SocketType::Rep, Some(10));

        // Short data frame header declaring a 20-byte body (flags=0x00, len=20),
        // which is over the 10-byte limit. Only the 2-byte header is needed: the
        // size check runs as soon as body_len is known.
        let events = session.on_bytes(Bytes::from_static(&[0x00, 20]));

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::Error(ZmtpError::SizeTooLarge))),
            "oversized frame should produce a SizeTooLarge error, got {} events",
            events.len()
        );
    }

    /// The same frame decodes cleanly when it is within the configured limit.
    #[test]
    fn active_session_accepts_frame_within_limit() {
        let mut session = ZmtpSession::new_active_with_max_frame_size(SocketType::Rep, Some(64));

        // flags=0x00, len=2, body="hi"
        let events = session.on_bytes(Bytes::from_static(&[0x00, 2, b'h', b'i']));

        assert!(
            events.iter().any(|e| matches!(e, SessionEvent::Frame(_))),
            "frame within the limit should decode, got {} events",
            events.len()
        );
    }

    fn valid_null_greeting() -> Bytes {
        let mut greeting = [0u8; 64];
        greeting[0] = 0xFF;
        greeting[9] = 0x7F;
        greeting[10] = 0x03;
        greeting[11] = 0x01;
        greeting[12..16].copy_from_slice(b"NULL");
        Bytes::copy_from_slice(&greeting)
    }

    fn input_with_handshake_command(command_body: Bytes) -> Bytes {
        let command_frame = encode_frame(FLAG_COMMAND, &command_body);
        let mut input = BytesMut::with_capacity(64 + command_frame.len());
        input.extend_from_slice(&valid_null_greeting());
        input.extend_from_slice(&command_frame);
        input.freeze()
    }

    fn has_protocol_error(events: &[SessionEvent]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, SessionEvent::Error(ZmtpError::Protocol)))
    }

    fn handshake_complete(events: &[SessionEvent]) -> Option<(SocketType, Option<Bytes>)> {
        events.iter().find_map(|event| match event {
            SessionEvent::HandshakeComplete {
                peer_socket_type,
                peer_identity,
            } => Some((*peer_socket_type, peer_identity.clone())),
            _ => None,
        })
    }

    #[test]
    fn session_rejects_non_ready_command_during_handshake() {
        let mut session = ZmtpSession::new(SocketType::Router);
        let input = input_with_handshake_command(Bytes::from_static(b"\x04PING"));
        let events = session.on_bytes(input);

        assert!(has_protocol_error(&events));
        assert!(handshake_complete(&events).is_none());
    }

    #[test]
    fn session_rejects_ready_without_socket_type() {
        let mut session = ZmtpSession::new(SocketType::Router);
        let input = input_with_handshake_command(Bytes::from_static(b"\x05READY"));
        let events = session.on_bytes(input);

        assert!(has_protocol_error(&events));
        assert!(handshake_complete(&events).is_none());
    }

    #[test]
    fn session_uses_socket_type_and_identity_from_ready_metadata() {
        let mut session = ZmtpSession::new(SocketType::Router);
        let input = input_with_handshake_command(build_ready("DEALER", Some(b"client-1")));
        let events = session.on_bytes(input);

        let (peer_socket_type, peer_identity) =
            handshake_complete(&events).expect("valid READY metadata should complete handshake");
        assert_eq!(peer_socket_type, SocketType::Dealer);
        assert_eq!(peer_identity.as_deref(), Some(&b"client-1"[..]));
    }
}
