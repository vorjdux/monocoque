use bytes::{BufMut, Bytes, BytesMut};

/// ZMTP frame flags
pub const FLAG_MORE: u8 = 0x01;
pub const FLAG_LONG: u8 = 0x02;
pub const FLAG_COMMAND: u8 = 0x04;

/// Encode a ZMTP frame header (flags + size).
///
/// Returns ONLY the header bytes. The body should be sent separately
/// using vectored writes to achieve zero-copy transmission.
///
/// Layout:
/// - Flags (1 byte)
/// - Size (1 byte if <= 255, else 8 bytes BE)
///
/// The caller must send the returned header followed by the body.
pub fn encode_frame_header(flags: u8, body_len: usize) -> Bytes {
    let out = if body_len <= 255 {
        let mut buf = BytesMut::with_capacity(2);
        buf.put_u8(flags & !FLAG_LONG);
        buf.put_u8(body_len as u8);
        buf
    } else {
        let mut buf = BytesMut::with_capacity(9);
        buf.put_u8(flags | FLAG_LONG);
        buf.put_u64(body_len as u64);
        buf
    };
    out.freeze()
}

/// Encode a complete ZMTP frame (header + body).
///
/// This function is used ONLY for protocol commands (READY, etc.) which are
/// small, infrequent messages sent during handshake. For user data messages,
/// use `encode_frame_header` to achieve zero-copy transmission.
///
/// Copies the body, but this is acceptable for protocol overhead.
pub fn encode_frame(flags: u8, body: &Bytes) -> Bytes {
    let len = body.len();
    let header_len = if len <= 255 { 2 } else { 9 };
    let mut out = BytesMut::with_capacity(header_len + len);

    if len <= 255 {
        out.put_u8(flags & !FLAG_LONG);
        out.put_u8(len as u8);
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
    let mut body = BytesMut::new();

    // Command name
    body.put_u8(5);
    body.extend_from_slice(b"READY");

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
    let name_bytes = name.as_bytes();

    dst.put_u8(name_bytes.len() as u8);
    dst.extend_from_slice(name_bytes);

    dst.put_u32(value.len() as u32);
    dst.extend_from_slice(value);
}
