//! Shared protocol parsing helpers for ZMTP security handshakes.

use crate::codec::ZmtpError;
use crate::session::SocketType;
use bytes::Bytes;
use compio_buf::BufResult;
use compio_io::AsyncRead;
use monocoque_core::timeout::read_exact_with_timeout;
use std::time::Duration;
use tracing::warn;

/// Read a fixed command prefix and verify it matches the expected bytes.
pub async fn read_command_prefix<S>(
    stream: &mut S,
    expected: &'static [u8],
    timeout: Option<Duration>,
) -> Result<(), ZmtpError>
where
    S: AsyncRead + Unpin,
{
    let header = vec![0u8; expected.len()];
    let buf_result = read_exact_with_timeout(stream, header, timeout)
        .await
        .map_err(ZmtpError::from)?;
    let BufResult(result, header) = buf_result;
    result?;

    if &header[..] != expected {
        return Err(ZmtpError::Protocol);
    }

    Ok(())
}

/// Reject bytes that arrive immediately after a command where no trailing data is expected.
pub async fn reject_immediately_available_trailing_bytes<S>(
    stream: &mut S,
    timeout: Duration,
) -> Result<(), ZmtpError>
where
    S: AsyncRead + Unpin,
{
    let trailing = vec![0u8; 1];
    read_exact_with_timeout(stream, trailing, Some(timeout))
        .await
        .map_or(Ok(()), |BufResult(result, _)| {
            result.map_or(Ok(()), |()| Err(ZmtpError::Protocol))
        })
}

/// Parse a READY command body and return the socket type and optional identity.
pub fn parse_ready_command(body: &Bytes) -> Result<(SocketType, Option<Bytes>), ZmtpError> {
    if body.len() < 6 {
        warn!(
            "[HANDSHAKE] ZMTP READY parse: body too short - got {} bytes, need at least 6",
            body.len()
        );
        return Err(ZmtpError::Protocol);
    }

    let name_len = body[0] as usize;
    if name_len != 5 || &body[1..6] != b"READY" {
        warn!(
            "[HANDSHAKE] ZMTP READY parse: expected command name \"READY\" (length=5), got length={} name={:?}",
            name_len,
            body.get(1..1 + name_len.min(body.len().saturating_sub(1)))
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default()
        );
        return Err(ZmtpError::Protocol);
    }

    let mut offset = 6;
    let mut socket_type = None;
    let mut identity = None;

    while offset < body.len() {
        let key_len = body[offset] as usize;
        offset += 1;

        if offset + key_len > body.len() {
            return Err(ZmtpError::Protocol);
        }

        let key = &body[offset..offset + key_len];
        offset += key_len;

        if offset + 4 > body.len() {
            return Err(ZmtpError::Protocol);
        }

        let value_len = u32::from_be_bytes([
            body[offset],
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + value_len > body.len() {
            return Err(ZmtpError::Protocol);
        }

        let value_start = offset;
        let value_end = offset + value_len;
        offset += value_len;

        match key {
            b"Socket-Type" => {
                if socket_type.is_some() {
                    return Err(ZmtpError::Protocol);
                }
                socket_type = Some(parse_socket_type(&body[value_start..value_end])?);
            }
            b"Identity" => {
                if identity.is_some() {
                    return Err(ZmtpError::Protocol);
                }
                if value_len > 255 {
                    return Err(ZmtpError::Protocol);
                }
                identity = Some(body.slice(value_start..value_end));
            }
            _ => {}
        }
    }

    let socket_type = socket_type.ok_or_else(|| {
        warn!("[HANDSHAKE] ZMTP READY parse: peer READY command is missing the required \"Socket-Type\" property");
        ZmtpError::Protocol
    })?;
    Ok((socket_type, identity))
}

fn parse_socket_type(bytes: &[u8]) -> Result<SocketType, ZmtpError> {
    let value = std::str::from_utf8(bytes).map_err(|_| ZmtpError::Protocol)?;
    match value {
        "PAIR" => Ok(SocketType::Pair),
        "DEALER" => Ok(SocketType::Dealer),
        "ROUTER" => Ok(SocketType::Router),
        "PUB" => Ok(SocketType::Pub),
        "SUB" => Ok(SocketType::Sub),
        "REQ" => Ok(SocketType::Req),
        "REP" => Ok(SocketType::Rep),
        "PUSH" => Ok(SocketType::Push),
        "PULL" => Ok(SocketType::Pull),
        "XPUB" => Ok(SocketType::Xpub),
        "XSUB" => Ok(SocketType::Xsub),
        _ => Err(ZmtpError::Protocol),
    }
}
