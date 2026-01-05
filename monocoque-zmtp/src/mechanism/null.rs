use crate::codec::{ZmtpError, ZmtpFrame};
use crate::mechanism::{require_command, Mechanism, Role};
use crate::session::SocketType;
use crate::utils::{build_ready, encode_frame, FLAG_COMMAND};
use bytes::{Buf, Bytes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NullState {
    /// Before we've emitted our READY (client usually sends immediately;
    /// server may also send immediately depending on your policy).
    NeedSendReady,
    /// Waiting for peer READY.
    NeedRecvReady,
    /// Handshake done.
    Done,
}

/// NULL mechanism for ZMTP 3.x (ZMTP/NULL).
///
/// Handshake logic here is simple:
/// - emit READY once
/// - accept peer READY once
/// - then Done
///
/// Notes:
/// - In real libzmq there are extra rules (version/greeting alignment, etc.)
/// - For Monocoque Phase 1/2: we enforce correctness enough for interop.
pub struct NullMechanism {
    #[allow(dead_code)]
    role: Role,
    local_socket_type: SocketType,

    state: NullState,

    // outbound queue (at most 1-2 frames during NULL)
    pending_out: Option<Bytes>,

    // captured peer info (owned)
    peer_socket_type: Option<SocketType>,
    peer_identity: Option<Bytes>,
}

impl NullMechanism {
    pub fn new(role: Role, local_socket_type: SocketType) -> Self {
        // Policy: send READY eagerly for both client and server.
        // If you later want strict ordering, keep state machine but gate `NeedSendReady`.
        let mut mech = Self {
            role,
            local_socket_type,
            state: NullState::NeedSendReady,
            pending_out: None,
            peer_socket_type: None,
            peer_identity: None,
        };

        // Preload first outbound READY
        mech.queue_ready();
        // After queueing READY, we expect to receive peer READY.
        mech.state = NullState::NeedRecvReady;

        mech
    }

    fn queue_ready(&mut self) {
        // Build READY command body with required metadata.
        let body = build_ready(self.local_socket_type.as_str(), None);

        // Wrap into a ZMTP frame: COMMAND flag set, body is READY command body.
        let framed = encode_frame(FLAG_COMMAND, &body);
        self.pending_out = Some(framed);
    }

    fn parse_ready_props(payload: &Bytes) -> Result<(SocketType, Option<Bytes>), ZmtpError> {
        // READY command frame body format (per ZMTP/37-ish):
        // [name_len: u8]["READY"] then properties repeated:
        //   [prop_name_len: u8][prop_name bytes][prop_value_len: u32 BE][prop_value bytes]
        //
        // We require Socket-Type prop.
        let mut buf = payload.clone();

        if buf.remaining() < 1 {
            return Err(ZmtpError::Protocol);
        }
        let name_len = buf.get_u8() as usize;
        if buf.remaining() < name_len {
            return Err(ZmtpError::Protocol);
        }
        let name = buf.copy_to_bytes(name_len);
        if name.as_ref() != b"READY" {
            return Err(ZmtpError::Protocol);
        }

        let mut socket_type: Option<SocketType> = None;
        let mut identity: Option<Bytes> = None;

        while buf.has_remaining() {
            if buf.remaining() < 1 {
                return Err(ZmtpError::Protocol);
            }
            let nlen = buf.get_u8() as usize;
            if buf.remaining() < nlen {
                return Err(ZmtpError::Protocol);
            }
            let pname = buf.copy_to_bytes(nlen);

            if buf.remaining() < 4 {
                return Err(ZmtpError::Protocol);
            }
            let vlen = buf.get_u32() as usize;
            if buf.remaining() < vlen {
                return Err(ZmtpError::Protocol);
            }
            let pval = buf.copy_to_bytes(vlen);

            match pname.as_ref() {
                b"Socket-Type" => {
                    socket_type = Some(SocketType::from_wire(&pval)?);
                }
                b"Identity" => {
                    // MUST be owned and stable; pval already owned Bytes.
                    identity = Some(pval);
                }
                _ => {
                    // ignore unknown props for forward compatibility
                }
            }
        }

        let st = socket_type.ok_or(ZmtpError::Protocol)?;
        Ok((st, identity))
    }
}

impl Mechanism for NullMechanism {
    fn on_inbound(&mut self, frame: &ZmtpFrame) -> Result<(), ZmtpError> {
        require_command(frame)?;

        if self.state != NullState::NeedRecvReady {
            // Receiving handshake frames out of order
            return Err(ZmtpError::Protocol);
        }

        // Expect READY command
        let (peer_type, peer_id) = Self::parse_ready_props(&frame.payload)?;
        self.peer_socket_type = Some(peer_type);
        self.peer_identity = peer_id;

        self.state = NullState::Done;
        Ok(())
    }

    fn next_outbound(&mut self) -> Option<Bytes> {
        self.pending_out.take()
    }

    fn is_done(&self) -> bool {
        self.state == NullState::Done
    }

    fn peer_identity(&self) -> Option<Bytes> {
        self.peer_identity.clone()
    }

    fn peer_socket_type(&self) -> Option<SocketType> {
        self.peer_socket_type
    }
}

// --- SocketType helpers ---
// Keep this in session.rs if you prefer; placed here for convenience.
impl SocketType {
    pub fn from_wire(b: &Bytes) -> Result<SocketType, ZmtpError> {
        match b.as_ref() {
            b"PAIR" => Ok(SocketType::Pair),
            b"DEALER" => Ok(SocketType::Dealer),
            b"ROUTER" => Ok(SocketType::Router),
            b"PUB" => Ok(SocketType::Pub),
            b"SUB" => Ok(SocketType::Sub),
            b"REQ" => Ok(SocketType::Req),
            b"REP" => Ok(SocketType::Rep),
            b"PUSH" => Ok(SocketType::Push),
            b"PULL" => Ok(SocketType::Pull),
            _ => Err(ZmtpError::Protocol),
        }
    }
}
