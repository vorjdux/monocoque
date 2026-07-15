#![no_main]

//! Fuzz the ZMTP READY command parser.
//!
//! `parse_ready_command` decodes the READY command body a peer sends during the
//! handshake: a length-prefixed command name followed by repeated
//! (name-len, name, 4-byte value-len, value) properties. Every length field is
//! attacker-controlled, so the parser must reject truncated, oversized, or
//! malformed property lists without panicking or over-reading; only `Ok`/`Err`.

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use monocoque_zmtp::parse_ready_command;

fuzz_target!(|data: &[u8]| {
    // Raw arbitrary bytes as a command body.
    let _ = parse_ready_command(&Bytes::copy_from_slice(data));

    // Also feed a well-formed "READY" prefix so the property loop past the
    // command name is exercised rather than being rejected up front.
    if !data.is_empty() {
        let mut body = Vec::with_capacity(data.len() + 6);
        body.push(5); // command-name length
        body.extend_from_slice(b"READY");
        body.extend_from_slice(data); // arbitrary property bytes
        let _ = parse_ready_command(&Bytes::from(body));
    }
});
