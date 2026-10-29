use bytes::{BufMut, Bytes, BytesMut};

/// ZMTP frame flags
pub const FLAG_LONG: u8 = 0x02;
pub const FLAG_COMMAND: u8 = 0x04;

/// Encode a complete ZMTP frame (header + body).
///
/// This function is used ONLY for protocol commands (READY, etc.) which are
/// small, infrequent messages sent during handshake. User data is framed by
/// the codec in [`crate::wire`].
pub fn encode_frame(flags: u8, body: &Bytes) -> Bytes {
    let len = body.len();
    let header_len = if len <= 255 { 2 } else { 9 };
    let mut out = BytesMut::with_capacity(header_len + len);

    if let Ok(short) = u8::try_from(len) {
        out.put_u8(flags & !FLAG_LONG);
        out.put_u8(short);
    } else {
        out.put_u8(flags | FLAG_LONG);
        out.put_u64(len as u64);
    }

    out.extend_from_slice(body);
    out.freeze()
}

/// Build a READY command body (ZMTP/37).
///
/// Grammar:
/// - 1 byte: command name length
/// - "READY"
/// - Repeated properties:
///   - 1 byte: property name length
///   - property name
///   - 4 bytes: value length (BE)
///   - value
///
/// Mandatory:
/// - Socket-Type
///
/// Optional:
/// - Identity
pub fn build_ready(socket_type: &str, identity: Option<&[u8]>) -> Bytes {
    build_metadata_command("READY", socket_type, identity)
}

/// Build the PLAIN `INITIATE` command (client side).
///
/// Identical metadata to `READY`; only the command name differs. ZMTP PLAIN has
/// the client send INITIATE after WELCOME and the server reply READY, so a PLAIN
/// client must not send a bare READY (libzmq rejects it).
pub fn build_initiate(socket_type: &str, identity: Option<&[u8]>) -> Bytes {
    build_metadata_command("INITIATE", socket_type, identity)
}

/// Build a metadata-carrying command (`READY` or `INITIATE`) body.
fn build_metadata_command(command: &str, socket_type: &str, identity: Option<&[u8]>) -> Bytes {
    let mut body = BytesMut::new();

    // Command name (length-prefixed). Only the fixed READY/INITIATE names reach here.
    body.put_u8(u8::try_from(command.len()).expect("ZMTP command name exceeds 255 bytes"));
    body.extend_from_slice(command.as_bytes());

    // Mandatory: Socket-Type
    put_property(&mut body, "Socket-Type", socket_type.as_bytes());

    // Optional: Identity
    if let Some(id) = identity {
        put_property(&mut body, "Identity", id);
    }

    body.freeze()
}

/// Helper: encode a READY property
#[inline]
fn put_property(dst: &mut BytesMut, name: &str, value: &[u8]) {
    // Names are the fixed RFC 23 property names; values are a socket-type name
    // or a routing identity, which the options layer caps at 255 bytes.
    let name_bytes = name.as_bytes();

    dst.put_u8(u8::try_from(name_bytes.len()).expect("ZMTP property name exceeds 255 bytes"));
    dst.extend_from_slice(name_bytes);

    dst.put_u32(u32::try_from(value.len()).expect("ZMTP property value exceeds u32"));
    dst.extend_from_slice(value);
}
