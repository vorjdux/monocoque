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

use crate::handshake::perform_handshake_with_options;
use crate::security::curve::CurveMessageCipher;
use crate::session::SocketType;
use bytes::{Buf, Bytes, BytesMut};
use monocoque_core::error::{MonocoqueError, Result};
use monocoque_core::msg::{FrameRef, INLINE_CAP, Msg};
use monocoque_core::mux::{Codec, OutQueue, PeerInfo, Protocol};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::PeerAddress;
use monocoque_core::transport::Stream;
use std::rc::Rc;
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

const PING: &[u8] = b"\x04PING";
const PONG: &[u8] = b"\x04PONG";
const SUBSCRIBE: &[u8] = b"\x09SUBSCRIBE";
const CANCEL: &[u8] = b"\x06CANCEL";
const ERROR: &[u8] = b"\x05ERROR";
const CURVE_MESSAGE: &[u8] = b"\x07MESSAGE";

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

    fn on_command(
        &mut self,
        body: &[u8],
        deliver: &mut dyn FnMut(Msg),
        replies: &mut OutQueue,
    ) -> Result<()> {
        if let Some(cipher) = self.cipher.as_mut()
            && body.starts_with(CURVE_MESSAGE)
        {
            let (more, payload) = cipher
                .decrypt_frame(body)
                .map_err(|e| MonocoqueError::Protocol(e.to_string()))?;
            return self.push_frame(payload, more, deliver);
        }
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
            put_command(replies, &pong);
        } else if body.starts_with(PONG) {
            // Any inbound traffic already refreshed the heartbeat timer.
        } else if let Some(topic) = body.strip_prefix(SUBSCRIBE) {
            // ZMTP 3.1 subscription commands surface the same way the 3.0
            // subscription messages do.
            let mut m = BytesMut::with_capacity(1 + topic.len());
            m.extend_from_slice(&[1]);
            m.extend_from_slice(topic);
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

/// Queue one command frame. Heartbeat commands travel unencrypted even on a
/// CURVE connection, as in libzmq.
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
                buf.reserve(total - buf.len());
                return Ok(());
            }

            let more = flags & FLAG_MORE != 0;
            if flags & FLAG_COMMAND != 0 {
                let body = buf.split_to(total);
                self.on_command(&body[header..], deliver, replies)?;
                continue;
            }
            if self.cipher.is_some() {
                return Err(MonocoqueError::Protocol(
                    "plaintext data frame on a CURVE connection".into(),
                ));
            }
            if !more && self.partial.is_empty() {
                if len <= INLINE_CAP {
                    deliver(Msg::copy_from_slice(&buf[header..total]));
                    buf.advance(total);
                } else {
                    buf.advance(header);
                    deliver(Msg::from(buf.split_to(len).freeze()));
                }
                continue;
            }
            buf.advance(header);
            let frame = buf.split_to(len).freeze();
            self.push_frame(frame, more, deliver)?;
        }
    }

    fn encode(&mut self, msg: &Msg, out: &mut OutQueue) -> Result<()> {
        if let Some(cipher) = self.cipher.as_mut() {
            let n = msg.len();
            for (i, frame) in msg.frames().enumerate() {
                let body = cipher
                    .encrypt_frame(frame, i + 1 < n)
                    .map_err(|e| MonocoqueError::Protocol(e.to_string()))?;
                put_header(out.buf(), FLAG_COMMAND, body.len());
                out.buf().extend_from_slice(&body);
            }
            return Ok(());
        }
        let n = msg.len();
        for (i, frame) in msg.frame_refs().enumerate() {
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
        self.cipher.is_none()
    }

    fn encode_ping(&mut self, ttl: Duration, out: &mut OutQueue) {
        let tenths = u16::try_from(ttl.as_millis() / 100).unwrap_or(u16::MAX);
        let mut body = [0u8; 7];
        body[..5].copy_from_slice(PING);
        body[5..].copy_from_slice(&tenths.to_be_bytes());
        put_command(out, &body);
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
            crate::codec::ZmtpError::AuthenticationFailed => MonocoqueError::AuthenticationFailed,
            other => MonocoqueError::Protocol(format!("handshake failed: {other}")),
        })?;
        let peer_type = result.peer_socket_type.as_str();
        let address = stream.zap_address();
        let mut metadata = vec![(
            "Socket-Type".to_string(),
            Bytes::from_static(peer_type.as_bytes()),
        )];
        if let Some(id) = &result.peer_identity {
            metadata.push(("Identity".to_string(), id.clone()));
        }
        if !address.is_empty() {
            metadata.push(("Peer-Address".to_string(), Bytes::from(address.clone())));
        }
        let info = PeerInfo {
            socket_type: peer_type,
            identity: result.peer_identity.filter(|id| !id.is_empty()),
            address,
            endpoint: None,
            metadata,
        };
        Ok((ZmtpCodec::new(result.curve_cipher, &options), info))
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
