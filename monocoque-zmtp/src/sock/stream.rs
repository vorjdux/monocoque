//! STREAM: raw TCP peers addressed by identity, with no ZMTP on the wire.
//!
//! Follows libzmq's `ZMQ_STREAM` framing. A receive yields `[identity, data]`;
//! when a peer connects or disconnects the socket yields `[identity, ""]`. A send
//! of `[identity, data...]` writes the data frames to that peer in order, and
//! `[identity, ""]` closes the connection.
//!
//! Received bytes are split off the read buffer as they arrived, with no copy;
//! sent frames go to the kernel as their own iovec above the vectored threshold.

use bytes::{Bytes, BytesMut};
use monocoque_core::error::{MonocoqueError, Result};
use monocoque_core::msg::{FrameRef, Msg, MsgRef};
use monocoque_core::mux::{
    Codec, Hooks, OutQueue, PeerId, PeerInfo, Protocol, SendTo, Shared, SocketCore, op,
};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::PeerAddress;
use monocoque_core::transport::Stream;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;
use std::task::Poll;

/// Raw byte streams: no handshake, no framing.
pub struct Raw;

/// Raw codec: every read is one message; every frame is written as is.
pub struct RawCodec {
    vectored_threshold: usize,
}

impl Codec for RawCodec {
    fn decode(
        &mut self,
        buf: &mut BytesMut,
        deliver: &mut dyn FnMut(Msg),
        _replies: &mut OutQueue,
    ) -> Result<()> {
        if !buf.is_empty() {
            let len = buf.len();
            deliver(Msg::from(buf.split_to(len).freeze()));
        }
        Ok(())
    }

    fn encode(&mut self, msg: &Msg, out: &mut OutQueue) -> Result<()> {
        for frame in msg.frame_refs() {
            match frame {
                FrameRef::Bytes(b) if b.len() >= self.vectored_threshold => {
                    out.push_segment(b.clone());
                }
                other => out.buf().extend_from_slice(other.as_slice()),
            }
        }
        Ok(())
    }

    fn shares_encoding(&self) -> bool {
        true
    }
}

impl Protocol for Raw {
    type Config = ();
    type Codec = RawCodec;

    async fn handshake(
        _cfg: Rc<()>,
        options: Rc<SocketOptions>,
        stream: Rc<Stream>,
        _outgoing: bool,
    ) -> Result<(RawCodec, PeerInfo)> {
        let address = stream.zap_address();
        Ok((
            RawCodec {
                vectored_threshold: options.vectored_write_threshold,
            },
            PeerInfo {
                socket_type: "STREAM",
                address,
                ..PeerInfo::default()
            },
        ))
    }

    fn socket_type(_cfg: &()) -> &'static str {
        "STREAM"
    }

    fn compatible(_cfg: &(), _peer: &str) -> bool {
        false
    }

    fn encode_shared(_cfg: &(), msg: &Msg, buf: &mut BytesMut) {
        for frame in msg.frames() {
            buf.extend_from_slice(frame);
        }
    }
}

/// Hashes the integer keys of STREAM's tables with one multiply. The keys
/// are chosen by this socket, never by a peer, so SipHash buys nothing.
#[derive(Default)]
struct IntHasher(u64);

impl Hasher for IntHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type IntMap<K, V> = HashMap<K, V, BuildHasherDefault<IntHasher>>;

/// The counter inside a STREAM identity, `None` for bytes this socket never
/// handed out.
fn identity_key(identity: &[u8]) -> Option<u32> {
    match identity {
        [0, a, b, c, d] => Some(u32::from_be_bytes([*a, *b, *c, *d])),
        _ => None,
    }
}

/// STREAM's identity table and its pending connect/disconnect notifications.
pub struct StreamHooks {
    by_identity: IntMap<u32, PeerId>,
    identity_of: IntMap<PeerId, Bytes>,
    next: u32,
    notify: bool,
    notifications: VecDeque<Msg>,
}

impl StreamHooks {
    fn notification(identity: &Bytes) -> Msg {
        Msg::from([identity.clone(), Bytes::new()])
    }
}

impl Hooks for StreamHooks {
    fn on_peer_up(&mut self, id: PeerId, _info: &PeerInfo) -> bool {
        self.next = self.next.wrapping_add(1);
        let mut raw = [0u8; 5];
        raw[1..].copy_from_slice(&self.next.to_be_bytes());
        let identity = Bytes::copy_from_slice(&raw);
        if self.notify {
            self.notifications.push_back(Self::notification(&identity));
        }
        self.by_identity.insert(self.next, id);
        self.identity_of.insert(id, identity);
        true
    }

    fn on_peer_down(&mut self, id: PeerId, _info: &PeerInfo) {
        if let Some(identity) = self.identity_of.remove(&id) {
            if let Some(key) = identity_key(&identity) {
                self.by_identity.remove(&key);
            }
            if self.notify {
                self.notifications.push_back(Self::notification(&identity));
            }
        }
    }
}

/// A STREAM socket. See the [module docs](self).
pub struct StreamSocket {
    pub(crate) core: Shared<Raw, StreamHooks>,
    slot: Msg,
}

/// `true` if a connect or disconnect notification is waiting to be received.
pub(crate) fn has_notification(core: &SocketCore<Raw, StreamHooks>) -> bool {
    !core.hooks().notifications.is_empty()
}

impl Default for StreamSocket {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamSocket {
    /// A STREAM socket with default options.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(SocketOptions::default())
    }

    /// A STREAM socket with `options`.
    #[must_use]
    pub fn with_options(options: SocketOptions) -> Self {
        let hooks = StreamHooks {
            by_identity: HashMap::default(),
            identity_of: HashMap::default(),
            next: rand::random(),
            notify: options.stream_notify,
            notifications: VecDeque::new(),
        };
        Self {
            core: SocketCore::new((), options, hooks),
            slot: Msg::new(),
        }
    }

    /// Bind a TCP or IPC endpoint and accept raw connections on it.
    pub async fn bind(&mut self, endpoint: &str) -> Result<String> {
        SocketCore::bind(&self.core, endpoint)
            .await
            .map(|e| e.to_string())
    }

    /// Connect to a raw TCP or IPC endpoint.
    pub async fn connect(&mut self, endpoint: &str) -> Result<()> {
        SocketCore::connect(&self.core, endpoint)
    }

    /// Number of connected peers.
    #[must_use]
    pub fn peer_count(&self) -> usize {
        self.core.borrow().peer_count()
    }

    /// Receive `[identity, data]`, or `[identity, ""]` for a connect or
    /// disconnect.
    pub async fn recv(&mut self) -> Result<MsgRef<'_>> {
        let timeout = self.core.borrow().options().recv_timeout;
        let slot = &mut self.slot;
        super::with_timeout(
            op(&self.core, move |c, cx| {
                loop {
                    c.progress();
                    if let Some(n) = c.hooks_mut().notifications.pop_front() {
                        *slot = n;
                        return Poll::Ready(());
                    }
                    match c.poll_recv(cx) {
                        Poll::Ready((peer, data)) => {
                            // Notifications raised while that receive ran come
                            // first, so a connect is reported before its data.
                            if let Some(identity) = c.hooks().identity_of.get(&peer).cloned() {
                                let mut m = data;
                                m.push_front(identity);
                                if let Some(n) = c.hooks_mut().notifications.pop_front() {
                                    c.hooks_mut().notifications.push_back(m);
                                    *slot = n;
                                } else {
                                    *slot = m;
                                }
                                return Poll::Ready(());
                            }
                        }
                        Poll::Pending => {
                            if c.hooks().notifications.is_empty() {
                                return Poll::Pending;
                            }
                        }
                    }
                }
            }),
            timeout,
        )
        .await?;
        Ok(MsgRef::new(&mut self.slot))
    }

    /// Receive and keep the next message.
    pub async fn recv_owned(&mut self) -> Result<Msg> {
        self.recv().await.map(MsgRef::take)
    }

    /// Send `[identity, data...]`; `[identity, ""]` closes that connection.
    /// An unknown identity fails with `HostUnreachable`.
    pub async fn send(&mut self, msg: impl Into<Msg>) -> Result<()> {
        let mut msg = msg.into();
        let identity = msg.pop_front().ok_or(MonocoqueError::InvalidRoutingId)?;
        let peer = identity_key(&identity)
            .and_then(|key| self.core.borrow().hooks().by_identity.get(&key).copied())
            .ok_or(MonocoqueError::HostUnreachable)?;
        if msg.byte_len() == 0 {
            self.core.borrow_mut().close_peer(peer);
            return Ok(());
        }
        match self.core.borrow_mut().send_to(peer, msg) {
            SendTo::Sent => Ok(()),
            SendTo::Full(_) => Err(MonocoqueError::Again),
            SendTo::Gone(_) => Err(MonocoqueError::HostUnreachable),
        }
    }

    /// Wait until everything queued has been written.
    pub async fn flush(&mut self) -> Result<()> {
        op(&self.core, SocketCore::poll_flushed).await;
        Ok(())
    }

    /// Close, waiting up to `linger` for queued bytes to be written.
    pub async fn close(mut self) -> Result<()> {
        let linger = self.core.borrow().options().linger;
        let drained = self.flush();
        match linger {
            Some(d) if d.is_zero() => {}
            Some(d) => {
                let _ = super::with_timeout(drained, Some(d)).await;
            }
            None => {
                drained.await?;
            }
        }
        Ok(())
    }
}

impl Drop for StreamSocket {
    fn drop(&mut self) {
        if let Ok(mut core) = self.core.try_borrow_mut() {
            core.close();
        }
    }
}
