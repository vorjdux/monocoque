//! Read buffers a reader keeps instead of returning to the allocator.
//!
//! A received message is a slice of the buffer it was read into, so the
//! buffer stays allocated until the last of its messages is dropped. A reader
//! on a busy stream needs room for its next read before that, takes a new
//! buffer, and the old one is freed a moment later. Freeing a buffer of that
//! size lets the allocator hand its pages back to the system, and the next
//! read then faults every page in again: a coalesced 256 B stream on tokio
//! took 217 000 page faults over six million messages and spent more time in
//! `recv` than the messages took to decode.
//!
//! The reader keeps the buffers it moves off instead, and takes one back once
//! its messages are gone. A steady stream then reads into the same few
//! buffers, already mapped and warm. They are let go when a read comes back
//! under half full, so a connection that goes idle keeps one read buffer.

use bytes::BytesMut;

/// Most buffers kept: the one the last read's messages still point into and
/// one that is free again, which is what a steady stream cycles through.
/// Messages queued up to a receive high-water mark can hold more alive at
/// once; the oldest is then let go and freed as before. It also bounds what a
/// connection can hold if it goes idle straight after a read that came back
/// more than half full, which [`Retired::after_read`] does not catch.
const KEPT: usize = 2;

/// Room a new buffer gets beyond what was asked for.
const SLACK: usize = 4096;

/// Buffers a reader moved off while messages still pointed into them.
#[derive(Default)]
pub struct Retired {
    kept: [Option<BytesMut>; KEPT],
    /// Slot the next retired buffer goes to.
    next: usize,
}

impl Retired {
    /// Give `buf` room for `additional` more bytes after what it holds.
    ///
    /// In order of preference: the room it has or can reclaim, a kept buffer
    /// whose messages are gone, a new allocation. `buf` is kept when it is
    /// replaced.
    pub fn make_room(&mut self, buf: &mut BytesMut, additional: usize) {
        if buf.try_reclaim(additional) {
            return;
        }
        let need = buf.len() + additional;
        // What `buf` holds is the start of a frame cut short by the read, a
        // different length every time. A page of slack lets one buffer serve
        // the next few without being a few bytes too small.
        let mut fresh = self
            .take(need)
            .unwrap_or_else(|| BytesMut::with_capacity((need + SLACK).next_multiple_of(SLACK)));
        fresh.extend_from_slice(buf);
        // The reader takes a read that fills its room exactly as the sign
        // that more is waiting, so it must see the room it asked for and not
        // the slack. The allocation keeps its full size for the next reuse.
        drop(fresh.split_off(need));
        let old = std::mem::replace(buf, fresh);
        if old.capacity() > 0 {
            self.kept[self.next] = Some(old);
            self.next = (self.next + 1) % KEPT;
        }
    }

    /// A kept buffer nothing points into any more, with room for `need`.
    fn take(&mut self, need: usize) -> Option<BytesMut> {
        self.kept.iter_mut().find_map(|slot| {
            let b = slot.as_mut()?;
            b.clear();
            if b.try_reclaim(need) {
                slot.take()
            } else {
                None
            }
        })
    }

    /// A read of `n` bytes came back with `room` to fill. One that used less
    /// than half of it means the stream is not keeping the reader busy, and
    /// the kept buffers go: an idle connection should hold one read buffer,
    /// not several. A busy stream's reads come back full or nearly so.
    pub fn after_read(&mut self, n: usize, room: usize) {
        if n < room / 2 {
            self.clear();
        }
    }

    /// Let every kept buffer go.
    pub fn clear(&mut self) {
        self.kept = Default::default();
    }

    /// `true` when no buffer is kept.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.kept.iter().all(Option::is_none)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    const READ: usize = 64 * 1024;

    /// Fill `buf`, split all but `left` bytes off as one message.
    fn read_and_split(buf: &mut BytesMut, left: usize) -> Bytes {
        let spare = buf.capacity() - buf.len();
        buf.resize(buf.len() + spare, 7);
        buf.split_to(buf.len() - left).freeze()
    }

    #[test]
    fn a_unique_buffer_is_reclaimed_in_place() {
        let mut retired = Retired::default();
        let mut buf = BytesMut::new();
        retired.make_room(&mut buf, READ);
        let ptr = buf.as_ptr();
        drop(read_and_split(&mut buf, 100));
        retired.make_room(&mut buf, READ);
        assert_eq!(buf.len(), 100);
        assert!(buf.capacity() - buf.len() >= READ);
        assert_eq!(buf.as_ptr(), ptr, "the same allocation, from its start");
        assert!(retired.is_empty());
    }

    #[test]
    fn a_stream_with_messages_outstanding_settles_on_three_buffers() {
        let mut retired = Retired::default();
        let mut buf = BytesMut::new();
        retired.make_room(&mut buf, READ);
        let mut seen = std::collections::HashSet::new();
        let mut held: Option<Bytes> = None;
        for round in 0..50 {
            // A read's messages are dropped only after the next read was
            // given its buffer, as when the user runs behind the reader: the
            // buffer being read into, the one before it, and one that is free.
            let msg = read_and_split(&mut buf, 100 + round);
            retired.make_room(&mut buf, READ);
            assert_eq!(buf.len(), 100 + round, "the partial frame is carried over");
            assert!(buf.iter().all(|&b| b == 7));
            assert_eq!(
                buf.capacity() - buf.len(),
                READ,
                "exactly the room asked for"
            );
            if round >= 3 {
                seen.insert(buf.as_ptr() as usize);
            }
            held = Some(msg);
        }
        drop(held);
        assert!(
            seen.len() <= 3,
            "{} allocations in steady state",
            seen.len()
        );
    }

    #[test]
    fn buffers_still_referenced_are_not_reused() {
        let mut retired = Retired::default();
        let mut buf = BytesMut::new();
        retired.make_room(&mut buf, READ);
        let mut held = Vec::new();
        for _ in 0..KEPT + 3 {
            let msg = read_and_split(&mut buf, 10);
            retired.make_room(&mut buf, READ);
            for m in &held {
                let m: &Bytes = m;
                assert!(m.iter().all(|&b| b == 7), "a held message was overwritten");
            }
            buf[..10].fill(7);
            held.push(msg);
        }
    }

    /// A burst that ends on a read neither full nor tiny leaves nothing kept.
    #[test]
    fn a_read_under_half_its_room_lets_the_kept_buffers_go() {
        let mut retired = Retired::default();
        let mut buf = BytesMut::new();
        retired.make_room(&mut buf, READ);
        let msg = read_and_split(&mut buf, 10);
        retired.make_room(&mut buf, READ);
        assert!(!retired.is_empty());
        retired.after_read(READ - 1, READ);
        assert!(!retired.is_empty(), "a nearly full read is a busy stream");
        retired.after_read(8 * 1024, READ);
        assert!(retired.is_empty());
        drop(msg);
    }

    #[test]
    fn clear_lets_the_kept_buffers_go() {
        let mut retired = Retired::default();
        let mut buf = BytesMut::new();
        retired.make_room(&mut buf, READ);
        let msg = read_and_split(&mut buf, 10);
        retired.make_room(&mut buf, READ);
        assert!(!retired.is_empty());
        retired.clear();
        assert!(retired.is_empty());
        drop(msg);
    }
}
