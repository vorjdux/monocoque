//! `Msg`, the one message type.
//!
//! A message is an ordered list of frames. The storage is chosen by shape so the
//! common cases never touch the heap or a refcount:
//!
//! - one frame of at most [`INLINE_CAP`] bytes lives inline in the `Msg` itself,
//!   so a small message costs no allocation and no atomic on either side, and
//!   holds no reference into the read buffer it was decoded from;
//! - one larger frame is a single [`Bytes`] (the dominant network case);
//! - up to four frames sit in a [`SmallVec`] with no allocation;
//! - [`Msg::into_shared`] moves the frames behind one `Arc` so a fan-out to N
//!   peers costs one allocation and N refcount bumps rather than N copies.
//!
//! [`MsgRef`] is what a borrowed receive hands out: a view of the socket's
//! current message that is released by dropping it. Keeping it costs nothing;
//! call [`MsgRef::take`] (or use the socket's `recv_owned`) to keep the message.

use bytes::Bytes;
use smallvec::SmallVec;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// Largest single frame stored inline, without a heap allocation.
pub const INLINE_CAP: usize = 64;

#[derive(Clone)]
enum FrameStore {
    Inline { len: u8, data: [u8; INLINE_CAP] },
    Single(Bytes),
    Multi(SmallVec<[Bytes; 4]>),
    Shared(Arc<[Bytes]>),
}

/// A multipart message.
#[derive(Clone)]
pub struct Msg {
    frames: FrameStore,
}

impl Msg {
    /// An empty message with no frames.
    #[must_use]
    pub fn new() -> Self {
        Self {
            frames: FrameStore::Multi(SmallVec::new()),
        }
    }

    /// A one-frame message holding a copy of `data`.
    ///
    /// Frames of at most [`INLINE_CAP`] bytes are stored inline with no heap
    /// allocation; longer ones are copied into a fresh `Bytes`.
    #[must_use]
    pub fn copy_from_slice(data: &[u8]) -> Self {
        if let Ok(len) = u8::try_from(data.len())
            && data.len() <= INLINE_CAP
        {
            let mut inline = [0u8; INLINE_CAP];
            inline[..data.len()].copy_from_slice(data);
            return Self {
                frames: FrameStore::Inline { len, data: inline },
            };
        }
        Self {
            frames: FrameStore::Single(Bytes::copy_from_slice(data)),
        }
    }

    /// Number of frames.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.frames {
            FrameStore::Inline { .. } | FrameStore::Single(_) => 1,
            FrameStore::Multi(v) => v.len(),
            FrameStore::Shared(v) => v.len(),
        }
    }

    /// `true` when the message has no frames.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total payload bytes across all frames.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.frames().map(<[u8]>::len).sum()
    }

    /// Frame `index`, or `None` past the end.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&[u8]> {
        match &self.frames {
            FrameStore::Inline { len, data } => (index == 0).then(|| &data[..usize::from(*len)]),
            FrameStore::Single(b) => (index == 0).then(|| &b[..]),
            FrameStore::Multi(v) => v.get(index).map(|b| &b[..]),
            FrameStore::Shared(v) => v.get(index).map(|b| &b[..]),
        }
    }

    /// The first frame, or `None` for an empty message.
    #[must_use]
    pub fn first(&self) -> Option<&[u8]> {
        self.get(0)
    }

    /// The last frame, or `None` for an empty message.
    #[must_use]
    pub fn last(&self) -> Option<&[u8]> {
        self.len().checked_sub(1).and_then(|i| self.get(i))
    }

    /// Iterate the frames as byte slices.
    pub fn frames(&self) -> Frames<'_> {
        Frames {
            msg: self,
            front: 0,
            back: self.len(),
        }
    }

    /// Frame `index` as `Bytes`: a refcount bump for a stored `Bytes`, a copy of
    /// at most [`INLINE_CAP`] bytes for an inline frame.
    #[must_use]
    pub fn frame_bytes(&self, index: usize) -> Option<Bytes> {
        match &self.frames {
            FrameStore::Inline { len, data } => {
                (index == 0).then(|| Bytes::copy_from_slice(&data[..usize::from(*len)]))
            }
            FrameStore::Single(b) => (index == 0).then(|| b.clone()),
            FrameStore::Multi(v) => v.get(index).cloned(),
            FrameStore::Shared(v) => v.get(index).cloned(),
        }
    }

    /// Append a frame.
    pub fn push(&mut self, frame: impl Into<Bytes>) {
        let frame = frame.into();
        if let FrameStore::Multi(v) = &mut self.frames {
            v.push(frame);
            return;
        }
        let mut v = self.take_vec();
        v.push(frame);
        self.set_vec(v);
    }

    /// Prepend a frame (a routing identity, an envelope delimiter).
    pub fn push_front(&mut self, frame: impl Into<Bytes>) {
        let frame = frame.into();
        if self.is_empty() {
            self.frames = FrameStore::Single(frame);
            return;
        }
        if let FrameStore::Multi(v) = &mut self.frames {
            v.insert(0, frame);
            return;
        }
        let mut v = self.take_vec();
        v.insert(0, frame);
        self.set_vec(v);
    }

    /// Remove and return the first frame.
    pub fn pop_front(&mut self) -> Option<Bytes> {
        if self.is_empty() {
            return None;
        }
        match &mut self.frames {
            FrameStore::Multi(v) if v.len() > 2 => Some(v.remove(0)),
            FrameStore::Multi(v) if v.len() == 2 => {
                let second = v.pop().expect("len checked");
                let first = v.pop().expect("len checked");
                self.frames = FrameStore::Single(second);
                Some(first)
            }
            FrameStore::Single(_) => {
                match std::mem::replace(&mut self.frames, FrameStore::Multi(SmallVec::new())) {
                    FrameStore::Single(b) => Some(b),
                    _ => unreachable!("matched Single"),
                }
            }
            _ => {
                let mut v = self.take_vec();
                let first = v.remove(0);
                self.set_vec(v);
                Some(first)
            }
        }
    }

    /// Store `v` in the cheapest shape that holds it.
    fn set_vec(&mut self, mut v: SmallVec<[Bytes; 4]>) {
        self.frames = if v.len() == 1 {
            FrameStore::Single(v.pop().expect("len checked"))
        } else {
            FrameStore::Multi(v)
        };
    }

    /// Remove every frame, keeping no allocation that a refcount would pin.
    pub fn clear(&mut self) {
        self.frames = FrameStore::Multi(SmallVec::new());
    }

    /// Move the frames behind one `Arc` so clones are a single refcount bump.
    ///
    /// Use it before handing the same message to many peers.
    #[must_use]
    pub fn into_shared(mut self) -> Self {
        if self.is_shared() {
            return self;
        }
        let frames: Arc<[Bytes]> = self.take_vec().into_iter().collect();
        Self {
            frames: FrameStore::Shared(frames),
        }
    }

    /// `true` if every clone of this message shares one frame list.
    #[must_use]
    pub const fn is_shared(&self) -> bool {
        matches!(self.frames, FrameStore::Shared(_))
    }

    /// `true` if the message is a single frame stored inline.
    #[must_use]
    pub const fn is_inline(&self) -> bool {
        matches!(self.frames, FrameStore::Inline { .. })
    }

    /// Consume the message, yielding its frames as `Bytes`.
    #[must_use]
    pub fn into_frames(mut self) -> SmallVec<[Bytes; 4]> {
        self.take_vec()
    }

    /// Take the frames out as an owned list, leaving the message empty.
    fn take_vec(&mut self) -> SmallVec<[Bytes; 4]> {
        match std::mem::replace(&mut self.frames, FrameStore::Multi(SmallVec::new())) {
            FrameStore::Inline { len, data } => {
                let mut v = SmallVec::new();
                v.push(Bytes::copy_from_slice(&data[..usize::from(len)]));
                v
            }
            FrameStore::Single(b) => {
                let mut v = SmallVec::new();
                v.push(b);
                v
            }
            FrameStore::Multi(v) => v,
            FrameStore::Shared(v) => v.iter().cloned().collect(),
        }
    }

    /// Iterate the frames as stored: a borrowed slice for an inline frame, the
    /// shared `Bytes` otherwise, so an encoder can hand a large body to the
    /// kernel without copying it.
    pub fn frame_refs(&self) -> impl Iterator<Item = FrameRef<'_>> + '_ {
        let (inline, stored): (Option<&[u8]>, &[Bytes]) = match &self.frames {
            FrameStore::Inline { len, data } => (Some(&data[..usize::from(*len)]), &[]),
            FrameStore::Single(b) => (None, std::slice::from_ref(b)),
            FrameStore::Multi(v) => (None, v.as_slice()),
            FrameStore::Shared(v) => (None, v),
        };
        inline
            .into_iter()
            .map(FrameRef::Slice)
            .chain(stored.iter().map(FrameRef::Bytes))
    }
}

/// One frame of a [`Msg`] as it is stored. See [`Msg::frame_refs`].
#[derive(Clone, Copy, Debug)]
pub enum FrameRef<'a> {
    /// An inline frame.
    Slice(&'a [u8]),
    /// A frame held as `Bytes`.
    Bytes(&'a Bytes),
}

impl FrameRef<'_> {
    /// The frame's bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Slice(s) => s,
            Self::Bytes(b) => b,
        }
    }
}

impl Default for Msg {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Bytes> for Msg {
    fn from(frame: Bytes) -> Self {
        Self {
            frames: FrameStore::Single(frame),
        }
    }
}

impl From<&'static [u8]> for Msg {
    fn from(frame: &'static [u8]) -> Self {
        Self::from(Bytes::from_static(frame))
    }
}

impl<const N: usize> From<&'static [u8; N]> for Msg {
    fn from(frame: &'static [u8; N]) -> Self {
        Self::from(Bytes::from_static(frame))
    }
}

impl From<&'static str> for Msg {
    fn from(frame: &'static str) -> Self {
        Self::from(Bytes::from_static(frame.as_bytes()))
    }
}

impl From<Vec<u8>> for Msg {
    fn from(frame: Vec<u8>) -> Self {
        Self::from(Bytes::from(frame))
    }
}

impl From<String> for Msg {
    fn from(frame: String) -> Self {
        Self::from(Bytes::from(frame))
    }
}

impl<const N: usize> From<[Bytes; N]> for Msg {
    fn from(frames: [Bytes; N]) -> Self {
        frames.into_iter().collect()
    }
}

impl FromIterator<Bytes> for Msg {
    fn from_iter<I: IntoIterator<Item = Bytes>>(iter: I) -> Self {
        let v: SmallVec<[Bytes; 4]> = iter.into_iter().collect();
        let frames = if v.len() == 1 {
            FrameStore::Single(v.into_iter().next().expect("len checked"))
        } else {
            FrameStore::Multi(v)
        };
        Self { frames }
    }
}

impl Extend<Bytes> for Msg {
    fn extend<I: IntoIterator<Item = Bytes>>(&mut self, iter: I) {
        for frame in iter {
            self.push(frame);
        }
    }
}

impl PartialEq for Msg {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.frames().eq(other.frames())
    }
}

impl Eq for Msg {}

impl<T: AsRef<[u8]>> PartialEq<[T]> for Msg {
    fn eq(&self, other: &[T]) -> bool {
        self.len() == other.len() && self.frames().zip(other).all(|(a, b)| a == b.as_ref())
    }
}

impl<T: AsRef<[u8]>, const N: usize> PartialEq<[T; N]> for Msg {
    fn eq(&self, other: &[T; N]) -> bool {
        self == &other[..]
    }
}

impl<T: AsRef<[u8]>> PartialEq<Vec<T>> for Msg {
    fn eq(&self, other: &Vec<T>) -> bool {
        self == &other[..]
    }
}

impl fmt::Debug for Msg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.frames().map(Bytes::copy_from_slice))
            .finish()
    }
}

/// Iterator over a message's frames. See [`Msg::frames`].
pub struct Frames<'a> {
    msg: &'a Msg,
    front: usize,
    back: usize,
}

impl<'a> Iterator for Frames<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.front == self.back {
            return None;
        }
        let frame = self.msg.get(self.front);
        self.front += 1;
        frame
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.back - self.front;
        (n, Some(n))
    }
}

impl DoubleEndedIterator for Frames<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        self.back -= 1;
        self.msg.get(self.back)
    }
}

impl ExactSizeIterator for Frames<'_> {}

/// Properties of the connection a message arrived on (`zmq_msg_gets`).
///
/// Built once per connection when its handshake completes and shared by every
/// message from it, so receiving costs nothing extra. Names compare ASCII
/// case-insensitively, as in libzmq. Typical properties are `Socket-Type`,
/// `Identity` (also reachable as `Routing-Id`), `Peer-Address`, `User-Id` (the
/// user id a ZAP handler returned) and whatever the peer put in its handshake
/// metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    props: Vec<(String, Bytes)>,
}

impl Metadata {
    /// An empty property set.
    #[must_use]
    pub const fn new() -> Self {
        Self { props: Vec::new() }
    }

    /// Set `name` to `value`, replacing an existing property of that name.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<Bytes>) {
        let name = name.into();
        let value = value.into();
        if let Some(slot) = self
            .props
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(&name))
        {
            slot.1 = value;
        } else {
            self.props.push((name, value));
        }
    }

    /// The raw value of `name`. `Routing-Id` is an alias for `Identity`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Bytes> {
        let find = |n: &str| {
            self.props
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .map(|(_, v)| v)
        };
        find(name).or_else(|| {
            if name.eq_ignore_ascii_case("Routing-Id") {
                find("Identity")
            } else {
                None
            }
        })
    }

    /// The value of `name` as text, like `zmq_msg_gets`. `None` if absent or
    /// not UTF-8.
    #[must_use]
    pub fn gets(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|v| std::str::from_utf8(v).ok())
    }

    /// Every property, in the order it was set.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Bytes)> {
        self.props.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of properties.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.props.len()
    }

    /// `true` if there are no properties.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.props.is_empty()
    }
}

/// A received message borrowed from the socket.
///
/// It derefs to [`Msg`]. Dropping it releases the socket's slot; nothing is
/// copied or refcounted to hand it out. [`take`](Self::take) moves the message
/// out when the caller needs to keep it past the next receive.
///
/// [`gets`](Self::gets) and [`metadata`](Self::metadata) read the properties
/// of the connection the message came from. They point at the socket's copy of
/// that connection's [`Metadata`]; a message does not carry it, so an owned
/// [`Msg`] taken out of a `MsgRef` has none (ask the socket instead).
pub struct MsgRef<'a> {
    msg: &'a mut Msg,
    meta: Option<&'a Metadata>,
}

// The whole-message wrappers sit on every receive; forcing them inline keeps
// the metadata pointer from costing a call on the hot path.
#[allow(clippy::inline_always)]
impl<'a> MsgRef<'a> {
    /// Wrap the socket's current message slot.
    #[inline(always)]
    pub const fn new(msg: &'a mut Msg) -> Self {
        Self { msg, meta: None }
    }

    /// Wrap the socket's current message slot along with the metadata of the
    /// connection it arrived on.
    #[inline(always)]
    pub const fn with_metadata(msg: &'a mut Msg, meta: Option<&'a Metadata>) -> Self {
        Self { msg, meta }
    }

    /// Move the message out, leaving the socket's slot empty.
    #[must_use]
    #[inline(always)]
    pub fn take(self) -> Msg {
        std::mem::take(self.msg)
    }

    /// The properties of the connection this message arrived on.
    #[must_use]
    #[inline]
    pub const fn metadata(&self) -> Option<&'a Metadata> {
        self.meta
    }

    /// One connection property as text (`zmq_msg_gets`).
    #[must_use]
    pub fn gets(&self, name: &str) -> Option<&'a str> {
        self.meta?.gets(name)
    }

    /// One connection property as raw bytes.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&'a Bytes> {
        self.meta?.get(name)
    }
}

impl Deref for MsgRef<'_> {
    type Target = Msg;

    fn deref(&self) -> &Msg {
        self.msg
    }
}

impl DerefMut for MsgRef<'_> {
    fn deref_mut(&mut self) -> &mut Msg {
        self.msg
    }
}

impl fmt::Debug for MsgRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized> PartialEq<T> for MsgRef<'_>
where
    Msg: PartialEq<T>,
{
    fn eq(&self, other: &T) -> bool {
        **self == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_lookup_ignores_case_aliases_routing_id_and_replaces() {
        let mut m = Metadata::new();
        m.insert("Identity", Bytes::from_static(b"a"));
        m.insert("X-Key", Bytes::from_static(b"1"));
        m.insert("x-key", Bytes::from_static(b"2"));
        assert_eq!(m.len(), 2);
        assert_eq!(m.gets("X-KEY"), Some("2"));
        assert_eq!(m.gets("Routing-Id"), Some("a"));
        assert_eq!(m.gets("Nope"), None);

        let mut slot = Msg::from("x");
        assert_eq!(MsgRef::new(&mut slot).gets("Identity"), None);
        assert_eq!(
            MsgRef::with_metadata(&mut slot, Some(&m)).gets("Identity"),
            Some("a")
        );
    }

    #[test]
    fn small_copy_is_inline_and_large_is_single() {
        let small = Msg::copy_from_slice(&[7u8; INLINE_CAP]);
        assert!(small.is_inline());
        assert_eq!(small.get(0), Some(&[7u8; INLINE_CAP][..]));
        let large = Msg::copy_from_slice(&[7u8; INLINE_CAP + 1]);
        assert!(!large.is_inline());
        assert_eq!(large.byte_len(), INLINE_CAP + 1);
    }

    #[test]
    fn push_and_pop_front_keep_frame_order() {
        let mut msg = Msg::new();
        assert!(msg.is_empty());
        msg.push(Bytes::from_static(b"b"));
        msg.push(Bytes::from_static(b"c"));
        msg.push_front(Bytes::from_static(b"a"));
        assert_eq!(msg, [b"a", b"b", b"c"]);
        assert_eq!(msg.pop_front().as_deref(), Some(&b"a"[..]));
        assert_eq!(msg, [b"b", b"c"]);
        assert_eq!(msg.pop_front().as_deref(), Some(&b"b"[..]));
        assert_eq!(msg, [b"c"]);
        assert_eq!(msg.pop_front().as_deref(), Some(&b"c"[..]));
        assert!(msg.pop_front().is_none());
    }

    #[test]
    fn inline_frame_survives_push() {
        let mut msg = Msg::copy_from_slice(b"head");
        msg.push(Bytes::from_static(b"tail"));
        assert_eq!(msg, [&b"head"[..], &b"tail"[..]]);
    }

    #[test]
    fn shared_clones_share_frames() {
        let msg: Msg = [Bytes::from_static(b"t"), Bytes::from_static(b"payload")].into();
        let shared = msg.into_shared();
        let copy = shared.clone();
        assert!(copy.is_shared());
        assert_eq!(copy, shared);
        assert_eq!(copy.last(), Some(&b"payload"[..]));
    }

    #[test]
    fn frames_iterates_both_ends() {
        let msg: Msg = [
            Bytes::from_static(b"1"),
            Bytes::from_static(b"2"),
            Bytes::from_static(b"3"),
        ]
        .into();
        let back: Vec<&[u8]> = msg.frames().rev().collect();
        assert_eq!(back, vec![&b"3"[..], b"2", b"1"]);
        assert_eq!(msg.frames().len(), 3);
    }

    #[test]
    fn msg_ref_take_moves_out() {
        let mut slot = Msg::from("hello");
        let r = MsgRef::new(&mut slot);
        assert_eq!(r, [b"hello"]);
        let owned = r.take();
        assert_eq!(owned, [b"hello"]);
        assert!(slot.is_empty());
    }

    #[test]
    fn frame_refs_expose_storage() {
        let msg: Msg = [Bytes::from_static(b"a"), Bytes::from_static(b"b")].into();
        let refs: Vec<FrameRef<'_>> = msg.frame_refs().collect();
        assert_eq!(refs.len(), 2);
        assert!(matches!(refs[0], FrameRef::Bytes(b) if b == &b"a"[..]));
        let inline = Msg::copy_from_slice(b"x");
        let refs: Vec<FrameRef<'_>> = inline.frame_refs().collect();
        assert!(matches!(refs[..], [FrameRef::Slice(b"x")]));
        assert!(Msg::new().frame_refs().next().is_none());
    }
}
