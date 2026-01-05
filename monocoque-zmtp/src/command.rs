use crate::codec::ZmtpError;
use bytes::Bytes;

/// Parsed ZMTP command (borrowed views into the payload).
#[derive(Debug, Clone)]
pub struct ZmtpCommand<'a> {
    pub name: &'a [u8],
    pub props: Vec<ZmtpProp<'a>>,
}

#[derive(Debug, Clone)]
pub struct ZmtpProp<'a> {
    pub name: &'a [u8],
    pub value: &'a [u8],
}

impl<'a> ZmtpCommand<'a> {
    pub fn name_str(&self) -> Option<&'a str> {
        std::str::from_utf8(self.name).ok()
    }

    pub fn get(&self, prop: &[u8]) -> Option<&'a [u8]> {
        self.props.iter().find(|p| p.name == prop).map(|p| p.value)
    }
}

/// Parse a command payload (frame body) into name + properties.
///
/// Input is the ZMTP frame payload (NOT including flags/size header).
///
/// Returns an owned struct with borrowed slices pointing into `payload`.
pub fn parse_command(payload: &Bytes) -> Result<ZmtpCommand<'_>, ZmtpError> {
    let mut i = 0;
    let b = payload.as_ref();

    // Need at least 1 byte for name_len
    if b.len() < 1 {
        return Err(ZmtpError::Protocol);
    }

    let name_len = b[0] as usize;
    i += 1;

    if b.len() < i + name_len {
        return Err(ZmtpError::Protocol);
    }

    let name = &b[i..i + name_len];
    i += name_len;

    let mut props = Vec::new();

    while i < b.len() {
        // prop name len
        if b.len() < i + 1 {
            return Err(ZmtpError::Protocol);
        }
        let pn_len = b[i] as usize;
        i += 1;

        if b.len() < i + pn_len {
            return Err(ZmtpError::Protocol);
        }
        let pn = &b[i..i + pn_len];
        i += pn_len;

        // value len (u32 BE)
        if b.len() < i + 4 {
            return Err(ZmtpError::Protocol);
        }
        let vl = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
        i += 4;

        if b.len() < i + vl {
            return Err(ZmtpError::Protocol);
        }
        let v = &b[i..i + vl];
        i += vl;

        props.push(ZmtpProp { name: pn, value: v });
    }

    Ok(ZmtpCommand { name, props })
}

/// Convenience: Parse READY metadata from a READY command.
#[derive(Debug, Clone)]
pub struct ReadyMeta<'a> {
    pub socket_type: Option<&'a [u8]>,
    pub identity: Option<&'a [u8]>,
}

pub fn parse_ready<'a>(cmd: &'a ZmtpCommand<'a>) -> ReadyMeta<'a> {
    // Spec property names: "Socket-Type", "Identity"
    let socket_type = cmd.get(b"Socket-Type");
    let identity = cmd.get(b"Identity");
    ReadyMeta {
        socket_type,
        identity,
    }
}

/// Helper: checks if command name matches ASCII (case-sensitive).
#[inline]
pub fn cmd_is(cmd: &ZmtpCommand<'_>, lit: &[u8]) -> bool {
    cmd.name == lit
}
