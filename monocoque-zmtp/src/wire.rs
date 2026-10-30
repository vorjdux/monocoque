//! ZMTP as a [`Protocol`] for the socket multiplexer.
//!
//! [`ZmtpCodec`] decodes straight out of a peer's contiguous read buffer into
//! [`Msg`]s. A single-frame message of at most `INLINE_CAP` bytes is copied into
//! the message itself, so it costs no allocation and no refcount and pins none
//! of the read buffer; longer frames are split off the buffer without a copy.
//! A frame longer than the bytes read so far reserves its full size up front,
//! so it is assembled in place by the following reads instead of growing a
//! staging buffer by doubling.
//!
//! On send, frames below the vectored-write threshold are copied into the
//! peer's outbound buffer; larger bodies go to the kernel as their own iovec.
//! CURVE encrypts every frame into a `MESSAGE` command.

use crate::handshake::{Framing, perform_handshake_with_options};
use crate::security::curve::{CurveMessageCipher, MESSAGE_FLAG_COMMAND, MESSAGE_FLAG_MORE};
use crate::session::SocketType;
use bytes::{Buf, Bytes, BytesMut};
use monocoque_core::error::{MonocoqueError, Result};
use monocoque_core::msg::{FrameRef, INLINE_CAP, Metadata, Msg};
use monocoque_core::mux::{Codec, OutQueue, PeerInfo, Protocol};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::PeerAddress;
use monocoque_core::transport::Stream;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const FLAG_MORE: u8 = 0x01;
const FLAG_LONG: u8 = 0x02;
const FLAG_COMMAND: u8 = 0x04;

/// Frames past this in one logical message fail the connection.
pub const MAX_FRAMES_PER_MESSAGE: usize = 8192;
/// Aggregate bytes past this in one logical message fail the connection.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;
/// Largest frame body accepted when `max_msg_size` is unset.
const DEFAULT_MAX_FRAME: usize = 64 * 1024 * 1024;
/// A frame at least this long that does not fit the read buffer is read into
/// a buffer of its own; see `ZmtpCodec::await_frame`.
const LONG_FRAME: usize = 8 * 1024;

const PING: &[u8] = b"\x04PING";
const PONG: &[u8] = b"\x04PONG";
const SUBSCRIBE: &[u8] = b"\x09SUBSCRIBE";
const CANCEL: &[u8] = b"\x06CANCEL";
const ERROR: &[u8] = b"\x05ERROR";
const CURVE_MESSAGE: &[u8] = b"\x07MESSAGE";
const JOIN: &[u8] = b"\x04JOIN";
const LEAVE: &[u8] = b"\x05LEAVE";

/// Per-socket ZMTP configuration.
#[derive(Clone, Copy, Debug)]
pub struct ZmtpConfig {
    /// This socket's type.
    pub socket_type: SocketType,
}

/// The ZMTP protocol marker.
pub struct Zmtp;

/// Write a frame header (flags + 1- or 8-byte length).
#[inline]
fn put_header(buf: &mut BytesMut, flags: u8, len: usize) {
    if let Ok(short) = u8::try_from(len) {
        buf.extend_from_slice(&[flags & !FLAG_LONG, short]);
    } else {
        buf.extend_from_slice(&[flags | FLAG_LONG]);
        buf.extend_from_slice(&(len as u64).to_be_bytes());
    }
}

/// Encode `msg` as plain ZMTP data frames into `buf`, copying every body.
pub fn encode_plain(msg: &Msg, buf: &mut BytesMut) {
    let n = msg.len();
    let total: usize = msg.frames().map(|f| f.len() + 9).sum();
    buf.reserve(total);
    for (i, frame) in msg.frames().enumerate() {
        let flags = if i + 1 < n { FLAG_MORE } else { 0 };
        put_header(buf, flags, frame.len());
        buf.extend_from_slice(frame);
    }
}

/// Per-connection ZMTP framing state.
pub struct ZmtpCodec {
    cipher: Option<CurveMessageCipher>,
    partial: Msg,
    partial_bytes: usize,
    max_frame: usize,
    vectored_threshold: usize,
    peer_ttl: Option<Duration>,
    framing: Framing,
    subscribe_all: bool,
    dish: bool,
    /// ZMTP 1.0 framing or DISH commands: anything but the plain path.
    unusual_encode: bool,
    /// The read buffer, set aside while a frame too long for it is read into
    /// a buffer of its own.
    parked: Option<BytesMut>,
}

impl ZmtpCodec {
    /// A codec for a connection whose handshake produced `cipher`.
    #[must_use]
    pub fn new(cipher: Option<CurveMessageCipher>, options: &SocketOptions) -> Self {
        Self {
            cipher,
            partial: Msg::new(),
            partial_bytes: 0,
            max_frame: options.max_msg_size.unwrap_or(DEFAULT_MAX_FRAME),
            vectored_threshold: options.vectored_write_threshold,
            peer_ttl: None,
            framing: Framing::V3,
            subscribe_all: false,
            dish: false,
            unusual_encode: false,
            parked: None,
        }
    }

    /// Make room for the `total`-byte frame whose start is in `buf`. One that
    /// fits the read buffer is assembled in place. One that does not gets a
    /// buffer of exactly its size, which the following reads fill and the
    /// frame then takes whole, while the read buffer waits in `parked`. That
    /// is one allocation per long frame, the frame's own, and the read buffer
    /// is neither regrown nor shared.
    fn await_frame(&mut self, buf: &mut BytesMut, total: usize) {
        if self.parked.is_none() && total >= LONG_FRAME && total > buf.capacity() {
            let mut frame = BytesMut::with_capacity(total);
            frame.extend_from_slice(buf);
            buf.clear();
            self.parked = Some(std::mem::replace(buf, frame));
        } else {
            buf.reserve(total - buf.len());
        }
    }

    /// Split the complete `total`-byte frame at the front of `buf` off.
    fn take_frame(&mut self, buf: &mut BytesMut, total: usize) -> BytesMut {
        match self.parked.take() {
            Some(mut read) => {
                read.extend_from_slice(&buf[total..]);
                buf.truncate(total);
                std::mem::replace(buf, read)
            }
            None => buf.split_to(total),
        }
    }

    /// Speak `framing` to a pre-ZMTP-3 peer. A ZMTP 1.0 subscriber filters
    /// for itself and sends no subscriptions, so a publisher (`publisher`)
    /// subscribes it to everything.
    #[must_use]
    pub fn with_framing(mut self, framing: Framing, publisher: bool) -> Self {
        self.framing = framing;
        self.subscribe_all = publisher && framing == Framing::V1;
        self.unusual_encode = self.dish || framing == Framing::V1;
        self
    }

    /// Encode for a DISH socket: everything it sends is a group change,
    /// which goes out as a JOIN or LEAVE command.
    #[must_use]
    pub fn for_dish(mut self) -> Self {
        self.dish = true;
        self.unusual_encode = true;
        self
    }

    /// `\x01group` becomes JOIN, `\x00group` LEAVE.
    fn encode_dish(&mut self, msg: &Msg, out: &mut OutQueue) -> Result<()> {
        let Some((&kind, group)) = msg.first().and_then(<[u8]>::split_first) else {
            return Ok(());
        };
        let name = if kind == 1 { JOIN } else { LEAVE };
        let mut body = BytesMut::with_capacity(name.len() + group.len());
        body.extend_from_slice(name);
        body.extend_from_slice(group);
        self.send_command(out, &body)
    }

    /// Decode ZMTP 1.0 frames: a length counting the flags byte (one byte,
    /// or 0xFF and eight), then the flags byte, then the body.
    fn decode_v1(&mut self, buf: &mut BytesMut, deliver: &mut dyn FnMut(Msg)) -> Result<()> {
        loop {
            let (header, counted, flags) = match buf.first() {
                None => return Ok(()),
                Some(0xFF) => {
                    if buf.len() < 10 {
                        return Ok(());
                    }
                    let raw = u64::from_be_bytes(buf[1..9].try_into().expect("8 bytes"));
                    let len =
                        usize::try_from(raw).map_err(|_| MonocoqueError::MessageTooLarge {
                            size: usize::MAX,
                            max: self.max_frame,
                        })?;
                    (10, len, buf[9])
                }
                Some(&len) => {
                    if buf.len() < 2 {
                        return Ok(());
                    }
                    (2, usize::from(len), buf[1])
                }
            };
            let len = counted
                .checked_sub(1)
                .ok_or_else(|| MonocoqueError::Protocol("ZMTP 1.0 frame without flags".into()))?;
            if len > self.max_frame {
                return Err(MonocoqueError::MessageTooLarge {
                    size: len,
                    max: self.max_frame,
                });
            }
            let total = header + len;
            if buf.len() < total {
                self.await_frame(buf, total);
                return Ok(());
            }
            let more = flags & FLAG_MORE != 0;
            if !more && self.partial.is_empty() && len <= INLINE_CAP {
                deliver(Msg::copy_from_slice(&buf[header..total]));
                buf.advance(total);
                continue;
            }
            let mut frame = self.take_frame(buf, total);
            frame.advance(header);
            self.push_frame(frame.freeze(), more, deliver)?;
        }
    }

    fn encode_v1<'a>(out: &mut OutQueue, n: usize, frames: impl Iterator<Item = FrameRef<'a>>) {
        for (i, frame) in frames.enumerate() {
            let flags = if i + 1 < n { FLAG_MORE } else { 0 };
            let body = frame.as_slice();
            let buf = out.buf();
            match u8::try_from(body.len() + 1) {
                Ok(short) if short < 0xFF => buf.extend_from_slice(&[short, flags]),
                _ => {
                    buf.extend_from_slice(&[0xFF]);
                    buf.extend_from_slice(&(body.len() as u64 + 1).to_be_bytes());
                    buf.extend_from_slice(&[flags]);
                }
            }
            buf.extend_from_slice(body);
        }
    }

    /// Account one more frame of the message being assembled.
    fn account(&mut self, len: usize) -> Result<()> {
        self.partial_bytes = self.partial_bytes.saturating_add(len);
        if self.partial.len() >= MAX_FRAMES_PER_MESSAGE {
            return Err(MonocoqueError::Protocol(
                "multipart message exceeds the per-message frame-count cap".into(),
            ));
        }
        if self.partial_bytes > MAX_MESSAGE_BYTES {
            return Err(MonocoqueError::MessageTooLarge {
                size: self.partial_bytes,
                max: MAX_MESSAGE_BYTES,
            });
        }
        Ok(())
    }

    /// Add one decoded data frame to the message being assembled.
    fn push_frame(&mut self, frame: Bytes, more: bool, deliver: &mut dyn FnMut(Msg)) -> Result<()> {
        self.account(frame.len())?;
        self.partial.push(frame);
        if !more {
            self.partial_bytes = 0;
            deliver(std::mem::take(&mut self.partial));
        }
        Ok(())
    }

    /// Decrypt one CURVE MESSAGE where it was read; the payload is a view of
    /// the read buffer.
    fn open_message(
        &mut self,
        body: BytesMut,
        deliver: &mut dyn FnMut(Msg),
        replies: &mut OutQueue,
    ) -> Result<()> {
        let Some(cipher) = self.cipher.as_mut() else {
            return Err(MonocoqueError::Protocol("MESSAGE without CURVE".into()));
        };
        let (flags, payload) = cipher
            .decrypt_frame_in_place(body)
            .map_err(|e| MonocoqueError::Protocol(e.to_string()))?;
        if flags & MESSAGE_FLAG_COMMAND != 0 {
            return self.on_command(&payload, deliver, replies);
        }
        self.push_frame(payload, flags & MESSAGE_FLAG_MORE != 0, deliver)
    }

    /// Queue one command, inside a MESSAGE box on a CURVE connection.
    fn send_command(&mut self, out: &mut OutQueue, body: &[u8]) -> Result<()> {
        let Some(cipher) = self.cipher.as_mut() else {
            put_command(out, body);
            return Ok(());
        };
        put_header(out.buf(), 0, CurveMessageCipher::message_len(body.len()));
        cipher
            .encrypt_command_into(body, out.buf())
            .map_err(|e| MonocoqueError::Protocol(e.to_string()))
    }

    fn on_command(
        &mut self,
        body: &[u8],
        deliver: &mut dyn FnMut(Msg),
        replies: &mut OutQueue,
    ) -> Result<()> {
        if let Some(rest) = body.strip_prefix(PING) {
            // PING: 2-byte TTL in tenths of a second, then up to 16 bytes of
            // context the PONG must echo (RFC 37).
            if rest.len() < 2 || rest.len() > 18 {
                return Err(MonocoqueError::Protocol("malformed PING".into()));
            }
            let ttl = u16::from_be_bytes([rest[0], rest[1]]);
            self.peer_ttl = (ttl != 0).then(|| Duration::from_millis(u64::from(ttl) * 100));
            let context = &rest[2..];
            let mut pong = BytesMut::with_capacity(PONG.len() + context.len());
            pong.extend_from_slice(PONG);
            pong.extend_from_slice(context);
            self.send_command(replies, &pong)?;
        } else if body.starts_with(PONG) {
            // Any inbound traffic already refreshed the heartbeat timer.
        } else if let Some(topic) = body.strip_prefix(SUBSCRIBE) {
            // ZMTP 3.1 subscription commands surface the same way the 3.0
            // subscription messages do.
            let mut m = BytesMut::with_capacity(1 + topic.len());
            m.extend_from_slice(&[1]);
            m.extend_from_slice(topic);
            deliver(Msg::from(m.freeze()));
        } else if let Some(group) = body.strip_prefix(JOIN) {
            // A DISH's group changes reach RADIO in subscription form.
            let mut m = BytesMut::with_capacity(1 + group.len());
            m.extend_from_slice(&[1]);
            m.extend_from_slice(group);
            deliver(Msg::from(m.freeze()));
        } else if let Some(group) = body.strip_prefix(LEAVE) {
            let mut m = BytesMut::with_capacity(1 + group.len());
            m.extend_from_slice(&[0]);
            m.extend_from_slice(group);
            deliver(Msg::from(m.freeze()));
        } else if let Some(topic) = body.strip_prefix(CANCEL) {
            let mut m = BytesMut::with_capacity(1 + topic.len());
            m.extend_from_slice(&[0]);
            m.extend_from_slice(topic);
            deliver(Msg::from(m.freeze()));
        } else if body.starts_with(ERROR) {
            return Err(MonocoqueError::Protocol("peer sent ERROR".into()));
        }
        Ok(())
    }
}

/// Queue one command frame in the clear (no CURVE; see `send_command`).
fn put_command(out: &mut OutQueue, body: &[u8]) {
    put_header(out.buf(), FLAG_COMMAND, body.len());
    out.buf().extend_from_slice(body);
}

impl Codec for ZmtpCodec {
    fn decode(
        &mut self,
        buf: &mut BytesMut,
        deliver: &mut dyn FnMut(Msg),
        replies: &mut OutQueue,
    ) -> Result<()> {
        if self.framing == Framing::V1 {
            return self.decode_v1(buf, deliver);
        }
        loop {
            if buf.len() < 2 {
                return Ok(());
            }
            let flags = buf[0];
            if flags & !(FLAG_MORE | FLAG_LONG | FLAG_COMMAND) != 0 {
                return Err(MonocoqueError::Protocol(
                    "reserved frame flag bits set".into(),
                ));
            }
            let (header, len) = if flags & FLAG_LONG != 0 {
                if buf.len() < 9 {
                    return Ok(());
                }
                let raw = u64::from_be_bytes(buf[1..9].try_into().expect("8 bytes"));
                let len = usize::try_from(raw).map_err(|_| MonocoqueError::MessageTooLarge {
                    size: usize::MAX,
                    max: self.max_frame,
                })?;
                (9, len)
            } else {
                (2, usize::from(buf[1]))
            };
            if len > self.max_frame {
                return Err(MonocoqueError::MessageTooLarge {
                    size: len,
                    max: self.max_frame,
                });
            }
            let total = header + len;
            if buf.len() < total {
                // Make room for the whole frame now so later reads land in
                // place rather than regrowing the buffer step by step.
                self.await_frame(buf, total);
                return Ok(());
            }

            let more = flags & FLAG_MORE != 0;
            if flags & FLAG_COMMAND != 0 {
                // A command is always a single frame.
                if more {
                    return Err(MonocoqueError::Protocol(
                        "command frame with the MORE flag set".into(),
                    ));
                }
                if self.framing == Framing::V2 {
                    return Err(MonocoqueError::Protocol(
                        "command frame from a ZMTP 2.0 peer".into(),
                    ));
                }
                let mut body = self.take_frame(buf, total);
                body.advance(header);
                if self.cipher.is_some() {
                    // Everything after a CURVE handshake is boxed; a command
                    // in the clear would be unauthenticated.
                    if !body.starts_with(CURVE_MESSAGE) {
                        return Err(MonocoqueError::Protocol(
                            "plaintext command on a CURVE connection".into(),
                        ));
                    }
                    self.open_message(body, deliver, replies)?;
                    continue;
                }
                self.on_command(&body, deliver, replies)?;
                continue;
            }
            if self.cipher.is_some() {
                // libzmq sends MESSAGE as a data frame; its own flags byte,
                // inside the box, says whether more frames follow.
                let mut body = self.take_frame(buf, total);
                body.advance(header);
                if !body.starts_with(CURVE_MESSAGE) {
                    return Err(MonocoqueError::Protocol(
                        "plaintext data frame on a CURVE connection".into(),
                    ));
                }
                self.open_message(body, deliver, replies)?;
                continue;
            }
            if !more && self.partial.is_empty() {
                if len <= INLINE_CAP {
                    deliver(Msg::copy_from_slice(&buf[header..total]));
                    buf.advance(total);
                } else {
                    let mut frame = self.take_frame(buf, total);
                    frame.advance(header);
                    deliver(Msg::from(frame.freeze()));
                }
                continue;
            }
            let mut frame = self.take_frame(buf, total);
            frame.advance(header);
            self.push_frame(frame.freeze(), more, deliver)?;
        }
    }

    fn filling_frame(&self) -> bool {
        self.parked.is_some()
    }

    fn encode(&mut self, msg: &Msg, out: &mut OutQueue) -> Result<()> {
        self.encode_prefixed(&[], msg, out)
    }

    fn encode_prefixed(&mut self, prefix: &[Bytes], msg: &Msg, out: &mut OutQueue) -> Result<()> {
        let n = prefix.len() + msg.len();
        let frames = prefix.iter().map(FrameRef::Bytes).chain(msg.frame_refs());
        if self.unusual_encode {
            if self.dish {
                return self.encode_dish(msg, out);
            }
            Self::encode_v1(out, n, frames);
            return Ok(());
        }
        if let Some(cipher) = self.cipher.as_mut() {
            for (i, frame) in frames.enumerate() {
                let body = frame.as_slice();
                // A data frame, as libzmq sends it, flagged MORE like the
                // frame it carries.
                let more = if i + 1 < n { FLAG_MORE } else { 0 };
                put_header(out.buf(), more, CurveMessageCipher::message_len(body.len()));
                cipher
                    .encrypt_frame_into(body, i + 1 < n, out.buf())
                    .map_err(|e| MonocoqueError::Protocol(e.to_string()))?;
            }
            return Ok(());
        }
        for (i, frame) in frames.enumerate() {
            let flags = if i + 1 < n { FLAG_MORE } else { 0 };
            let body = frame.as_slice();
            put_header(out.buf(), flags, body.len());
            match frame {
                FrameRef::Bytes(b) if b.len() >= self.vectored_threshold => {
                    out.push_segment(b.clone());
                }
                _ => out.buf().extend_from_slice(body),
            }
        }
        Ok(())
    }

    fn shares_encoding(&self) -> bool {
        self.cipher.is_none() && self.framing != Framing::V1
    }

    fn initial_message(&mut self) -> Option<Msg> {
        std::mem::take(&mut self.subscribe_all).then(|| Msg::from(Bytes::from_static(b"\x01")))
    }

    fn encode_ping(&mut self, ttl: Duration, out: &mut OutQueue) {
        // ZMTP 1.0 and 2.0 have no heartbeat commands.
        if self.framing != Framing::V3 {
            return;
        }
        let tenths = u16::try_from(ttl.as_millis() / 100).unwrap_or(u16::MAX);
        let mut body = [0u8; 7];
        body[..5].copy_from_slice(PING);
        body[5..].copy_from_slice(&tenths.to_be_bytes());
        // Only fails when the CURVE send nonce is exhausted, and the next
        // data send reports that.
        let _ = self.send_command(out, &body);
    }

    fn peer_ttl(&self) -> Option<Duration> {
        self.peer_ttl
    }
}

impl Protocol for Zmtp {
    type Config = ZmtpConfig;
    type Codec = ZmtpCodec;

    async fn handshake(
        cfg: Rc<ZmtpConfig>,
        options: Rc<SocketOptions>,
        stream: Rc<Stream>,
        _outgoing: bool,
    ) -> Result<(ZmtpCodec, PeerInfo)> {
        let mut s = &*stream;
        let result = perform_handshake_with_options(
            &mut s,
            cfg.socket_type,
            options.routing_id.as_deref(),
            Some(options.handshake_timeout),
            &options,
        )
        .await
        .map_err(|e| match e {
            crate::error::ZmtpError::AuthenticationFailed => MonocoqueError::AuthenticationFailed,
            other => MonocoqueError::Protocol(format!("handshake failed: {other}")),
        })?;
        let peer_type = result.peer_socket_type.as_str();
        let address = stream.zap_address();
        // Built once per connection; every message received from this peer
        // shares it through the socket's receive slot.
        let mut metadata = Metadata::new();
        for (k, v) in result.properties {
            metadata.insert(k, v);
        }
        metadata.insert("Socket-Type", Bytes::from_static(peer_type.as_bytes()));
        if let Some(id) = result.peer_identity.as_ref().filter(|id| !id.is_empty()) {
            metadata.insert("Identity", id.clone());
        }
        if !address.is_empty() {
            metadata.insert("Peer-Address", Bytes::from(address.clone()));
        }
        if let Some(user) = result.user_id {
            metadata.insert("User-Id", Bytes::from(user));
        }
        let info = PeerInfo {
            socket_type: peer_type,
            identity: result.peer_identity.filter(|id| !id.is_empty()),
            address,
            endpoint: None,
            metadata: Arc::new(metadata),
        };
        let publisher = matches!(cfg.socket_type, SocketType::Pub | SocketType::Xpub);
        let dish = cfg.socket_type == SocketType::Dish;
        let mut codec =
            ZmtpCodec::new(result.curve_cipher, &options).with_framing(result.framing, publisher);
        if dish {
            codec = codec.for_dish();
        }
        Ok((codec, info))
    }

    fn socket_type(cfg: &ZmtpConfig) -> &'static str {
        cfg.socket_type.as_str()
    }

    fn compatible(cfg: &ZmtpConfig, peer: &str) -> bool {
        crate::handshake::socket_types_compatible(cfg.socket_type, peer)
    }

    fn encode_shared(_cfg: &ZmtpConfig, msg: &Msg, buf: &mut BytesMut) {
        encode_plain(msg, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(codec: &mut ZmtpCodec, wire: &[u8]) -> Result<Vec<Msg>> {
        let mut buf = BytesMut::from(wire);
        let mut out = Vec::new();
        let mut replies = OutQueue::default();
        codec.decode(&mut buf, &mut |m| out.push(m), &mut replies)?;
        assert!(buf.is_empty(), "undecoded bytes left: {buf:?}");
        Ok(out)
    }

    fn codec() -> ZmtpCodec {
        ZmtpCodec::new(None, &SocketOptions::default())
    }

    #[test]
    fn round_trips_single_and_multipart_messages() {
        let mut c = codec();
        let msgs = [
            Msg::from("hi"),
            Msg::from(vec![7u8; 300]),
            [
                Bytes::from_static(b"id"),
                Bytes::new(),
                Bytes::from(vec![1u8; 70_000]),
            ]
            .into(),
        ];
        let mut wire = BytesMut::new();
        for m in &msgs {
            encode_plain(m, &mut wire);
        }
        let decoded = decode_all(&mut c, &wire).unwrap();
        assert_eq!(decoded.len(), 3);
        for (a, b) in decoded.iter().zip(&msgs) {
            assert_eq!(a, b);
        }
        assert!(
            decoded[0].is_inline(),
            "a small single frame decodes inline"
        );
    }

    #[test]
    fn partial_frame_reserves_its_full_length() {
        let mut c = codec();
        let mut wire = BytesMut::new();
        encode_plain(&Msg::from(vec![3u8; 16 * 1024]), &mut wire);
        let mut buf = BytesMut::from(&wire[..100]);
        let mut out = Vec::new();
        let mut replies = OutQueue::default();
        c.decode(&mut buf, &mut |m| out.push(m), &mut replies)
            .unwrap();
        assert!(out.is_empty());
        assert!(
            buf.capacity() >= wire.len(),
            "frame did not reserve its size"
        );
        buf.extend_from_slice(&wire[100..]);
        c.decode(&mut buf, &mut |m| out.push(m), &mut replies)
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].byte_len(), 16 * 1024);
    }

    #[test]
    fn ping_is_answered_with_a_pong_echoing_its_context() {
        let mut c = codec();
        let mut wire = BytesMut::new();
        put_header(&mut wire, FLAG_COMMAND, 9);
        wire.extend_from_slice(b"\x04PING\x00\x32ab");
        let mut buf = wire;
        let mut replies = OutQueue::default();
        c.decode(&mut buf, &mut |_| unreachable!(), &mut replies)
            .unwrap();
        assert_eq!(&replies.buf()[..], b"\x04\x07\x04PONGab");
        assert_eq!(c.peer_ttl(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn subscribe_command_surfaces_as_a_subscription_message() {
        let mut c = codec();
        let mut wire = BytesMut::new();
        put_header(&mut wire, FLAG_COMMAND, SUBSCRIBE.len() + 3);
        wire.extend_from_slice(SUBSCRIBE);
        wire.extend_from_slice(b"abc");
        let got = decode_all(&mut c, &wire).unwrap();
        assert_eq!(got, vec![Msg::from("\x01abc")]);
    }

    #[test]
    fn caps_frame_count_and_rejects_reserved_bits() {
        let mut c = codec();
        let mut wire = BytesMut::new();
        for _ in 0..=MAX_FRAMES_PER_MESSAGE {
            put_header(&mut wire, FLAG_MORE, 1);
            wire.extend_from_slice(b"x");
        }
        assert!(decode_all(&mut c, &wire).is_err());
        assert!(decode_all(&mut codec(), &[0x80, 0]).is_err());
    }

    #[test]
    fn rejects_frames_over_max_msg_size() {
        let mut c = ZmtpCodec::new(None, &SocketOptions::default().with_max_msg_size(Some(10)));
        let mut wire = BytesMut::new();
        put_header(&mut wire, 0, 11);
        wire.extend_from_slice(&[0; 11]);
        assert!(matches!(
            decode_all(&mut c, &wire),
            Err(MonocoqueError::MessageTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_long_length_that_would_truncate_past_the_cap() {
        // (1 << 32) | 100 narrows to 100 on a 32-bit usize; the length must be
        // rejected before it is narrowed, or framing desyncs.
        let mut c = ZmtpCodec::new(
            None,
            &SocketOptions::default().with_max_msg_size(Some(1000)),
        );
        let mut wire = vec![FLAG_LONG];
        wire.extend_from_slice(&((1u64 << 32) + 100).to_be_bytes());
        assert!(matches!(
            decode_all(&mut c, &wire),
            Err(MonocoqueError::MessageTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_command_frame_with_more_flag() {
        let wire = b"\x05\x06\x05READY";
        assert!(matches!(
            decode_all(&mut codec(), wire),
            Err(MonocoqueError::Protocol(_))
        ));
    }

    #[test]
    fn large_frames_go_out_as_their_own_iovec() {
        let mut c = codec();
        let mut out = OutQueue::default();
        let body = Bytes::from(vec![5u8; 1 << 20]);
        c.encode(&Msg::from(body), &mut out).unwrap();
        let (mut spare, mut iov) = (BytesMut::new(), Vec::new());
        match out.take_job(&mut spare, &mut iov).unwrap() {
            monocoque_core::mux::WriteJob::Vectored(v) => {
                assert_eq!(v.len(), 2, "header then body");
                assert_eq!(v[1].len(), 1 << 20);
            }
            monocoque_core::mux::WriteJob::Contiguous(_) => panic!("large body was copied"),
        }
    }
}
