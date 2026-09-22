//! The input a fed decoder is holding, as the pieces it was given.
//!
//! A decoder fed from a download is handed one buffer after another, and the
//! runs it cuts out of them do not line up with them: a run starts in the
//! middle of one piece and ends in the middle of another three pieces later.
//! The obvious way to serve that is to copy every piece into one growing
//! buffer and hand workers slices of it, and it is what this decoder used to
//! do. It costs two copies of every byte in the stream - once into the buffer,
//! once out of it into the run a worker is given - and a buffer whose capacity
//! is charged against the memory limit long after the bytes in it were
//! claimed.
//!
//! So the pieces are kept as they arrived. Each is reference-counted, a run is
//! the list of pieces it spans, and a worker is handed those references rather
//! than a copy of the bytes. A piece is let go when the cursor has passed it
//! and no worker still holds it, which the reference count answers exactly.
//! Nothing here copies: the only memory that moves is what a caller hands over
//! and what it gets back.
//!
//! C: nothing. The C decoder reads through a callback into one buffer, which
//! is the shape this replaces.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::ops::Range;
use std::sync::Arc;

/// The bytes of one piece of input, behind a handle only this queue hands out.
///
/// The queue frees a piece when the last holder lets go, and what it counts
/// holders with is the reference count. That has to be a count of the queue's
/// own holders and nothing else, so the bytes are wrapped in a handle the
/// queue creates: a caller that lends a buffer it is keeping goes on holding
/// its own reference to the buffer, which this count cannot see, and two
/// ranges lent out of one buffer get a handle each rather than inflating each
/// other's count.
pub(crate) struct SegData {
    data: Arc<Vec<u8>>,
}

/// One piece of input, and where it sits in the stream.
#[derive(Clone)]
pub(crate) struct Seg {
    /// The bytes, behind the queue's own handle to them.
    data: Arc<SegData>,
    /// The part of `data` that belongs to the stream.
    pub(crate) range: Range<usize>,
    /// The stream offset of `data[range.start]`.
    pub(crate) start: u64,
    /// What this piece is charged against the memory limit. An owned piece is
    /// charged for the whole allocation, because the decoder is what keeps it
    /// alive; a piece shared with a caller who is keeping it anyway is charged
    /// for the bytes lent, because that is what the decoder added to the
    /// caller's footprint.
    charge: u64,
    /// Whether the allocation is the decoder's own, and so whether it can be
    /// kept for the next piece when the stream is past this one. A range lent
    /// by a caller is not the decoder's to keep; a copy it made, or a buffer
    /// handed over outright, is.
    owned: bool,
}

impl Seg {
    /// The bytes this piece still holds.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.data.data[self.range.clone()]
    }

    /// One past the last stream offset in this piece.
    fn end(&self) -> u64 {
        self.start + (self.range.end - self.range.start) as u64
    }
}

/// The pieces of input a decoder is holding, in stream order.
#[derive(Default)]
pub(crate) struct SegQueue {
    q: VecDeque<Seg>,
    /// Pieces the queue has finished with that a worker has not: dropped from
    /// the queue proper, still charged, swept when the last holder lets go.
    lent: Vec<Seg>,
    /// The stream offset of the first byte still held.
    base: u64,
    /// One past the last stream offset held.
    end: u64,
    /// What every piece above is charged, together, plus the parked
    /// allocations below.
    held: u64,
    /// Allocations the stream is past, kept for the next piece rather than
    /// given back to the allocator.
    ///
    /// Copying a chunk of input into a fresh allocation and freeing it once
    /// its runs are claimed costs a page fault per page on the way in and a
    /// call to the allocator each way. On a stream whose runs are large that
    /// was the whole cost of holding input as pieces: a gigabyte of input is
    /// two hundred and fifty thousand faults that a reused buffer does not
    /// pay. So a piece of the decoder's own is emptied and kept, and the next
    /// copy goes into it.
    spare: Vec<Vec<u8>>,
    /// What the parked allocations cost, counted inside `held`.
    spare_bytes: u64,
    /// The most that may sit parked, and in how many buffers. Set by the
    /// decoder, which is what knows the memory limit.
    park_room: u64,
    park_slots: usize,
    /// The length of the last piece taken: what the next one is expected to
    /// ask for, and so the size a parked buffer has to be near to be worth
    /// keeping.
    last_piece: u64,
}

impl SegQueue {
    /// Takes a piece of input, owned outright.
    pub(crate) fn push_owned(&mut self, seg: Vec<u8>) {
        let charge = seg.capacity() as u64;
        let range = 0..seg.len();
        self.push(Arc::new(seg), range, charge, true);
    }

    /// Takes a piece of input the caller is keeping a reference to.
    pub(crate) fn push_shared(&mut self, data: &Arc<Vec<u8>>, range: Range<usize>) {
        let charge = (range.end - range.start) as u64;
        self.push(Arc::clone(data), range, charge, false);
    }

    fn push(&mut self, data: Arc<Vec<u8>>, range: Range<usize>, charge: u64, owned: bool) {
        debug_assert!(range.end <= data.len());
        let data = Arc::new(SegData { data });
        if range.is_empty() {
            return;
        }
        let len = (range.end - range.start) as u64;
        let start = self.end;
        self.q.push_back(Seg {
            data,
            range,
            start,
            charge,
            owned,
        });
        self.end += len;
        self.held += charge;
        self.last_piece = len;
    }

    /// Says how much may sit parked: at most `room` bytes in at most `slots`
    /// buffers. The decoder sets this, because what a small part of the limit
    /// is is the decoder's arithmetic and not the queue's.
    pub(crate) fn set_park_budget(&mut self, room: u64, slots: usize) {
        self.park_room = room;
        self.park_slots = slots;
    }

    /// An allocation the stream is past, emptied, for the next piece.
    ///
    /// Only ever a buffer nothing holds any more: a piece the decoder or one
    /// of its workers is still reading is never handed out here.
    pub(crate) fn take_spare(&mut self) -> Option<Vec<u8>> {
        let buf = self.spare.pop()?;
        let cap = buf.capacity() as u64;
        self.spare_bytes -= cap;
        self.held -= cap;
        Some(buf)
    }

    /// The length of the last piece taken.
    pub(crate) fn last_piece(&self) -> u64 {
        self.last_piece
    }

    /// One past the last stream offset held.
    pub(crate) fn end(&self) -> u64 {
        self.end
    }

    /// How many bytes are held between the first and the last.
    pub(crate) fn len(&self) -> u64 {
        self.end - self.base
    }

    /// What is charged for and cannot be decoded: the front of a piece the
    /// cursor has passed but whose end it has not, and pieces a worker still
    /// holds. Transient, and the measure of how much room a budget has to
    /// leave beyond the run it wants to hold.
    pub(crate) fn dead_bytes(&self) -> u64 {
        self.held
            .saturating_sub(self.len())
            .saturating_sub(self.spare_bytes)
    }

    /// What every piece the decoder is keeping alive costs, whether it is
    /// still in the queue or out with a worker, plus what sits parked.
    pub(crate) fn held_bytes(&self) -> u64 {
        self.held
    }

    /// What sits parked, waiting to be filled again.
    ///
    /// Charged, because it is memory the decoder is holding, but not an
    /// obstacle to taking more input: the next piece copied goes into it, so
    /// what it costs is already counted against the piece it will become.
    pub(crate) fn spare_bytes(&self) -> u64 {
        self.spare_bytes
    }

    /// The contiguous run of bytes that starts at `offset`, or nothing when
    /// the stream has not reached it yet.
    ///
    /// A caller that wants more than this asks again from where this piece
    /// ends: the decoders on both paths take input a piece at a time and keep
    /// their state between pieces, so a boundary costs a second call and
    /// nothing else.
    pub(crate) fn piece_at(&self, offset: u64) -> Option<&[u8]> {
        if offset < self.base || offset >= self.end {
            return None;
        }
        let seg = self.q.get(self.index_of(offset)?)?;
        let skip = (offset - seg.start) as usize;
        Some(&seg.data.data[seg.range.start + skip..seg.range.end])
    }

    /// Where in the queue the piece holding `offset` sits.
    ///
    /// The pieces are in stream order and do not overlap, so this is a search
    /// and not a walk. It has to be: a decoder holding a gigabyte of a stream
    /// that arrived in chunks holds hundreds of pieces, and a walk from the
    /// front on every step of the decode is quadratic in the number of pieces,
    /// which showed up as a fifth of the instructions on a stream of very
    /// large runs before this was a search.
    fn index_of(&self, offset: u64) -> Option<usize> {
        let (mut lo, mut hi) = (0usize, self.q.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let seg = &self.q[mid];
            if offset < seg.start {
                hi = mid;
            } else if offset >= seg.end() {
                lo = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }

    /// The pieces spanning `range`, as references a worker can be given.
    ///
    /// Returns nothing if the range is not wholly held, so a caller cannot be
    /// handed half a run.
    pub(crate) fn claim(&self, range: Range<u64>) -> Option<Vec<Seg>> {
        if range.start < self.base || range.end > self.end || range.start >= range.end {
            return None;
        }
        let mut out = Vec::new();
        let first = self.index_of(range.start)?;
        for seg in self.q.iter().skip(first) {
            if seg.start >= range.end {
                break;
            }
            let from = range.start.max(seg.start) - seg.start;
            let to = range.end.min(seg.end()) - seg.start;
            out.push(Seg {
                data: Arc::clone(&seg.data),
                range: seg.range.start + from as usize..seg.range.start + to as usize,
                start: seg.start + from,
                // A claimed piece is a second reference to bytes the queue is
                // already charged for. Charging it again would count the same
                // memory twice; what keeps it honest is that the queue goes on
                // charging until the last holder is gone.
                charge: 0,
                // A claim is a reader of someone else's allocation, whatever
                // that allocation is: nothing it drops is the queue's to keep.
                owned: false,
            });
        }
        Some(out)
    }

    /// Drops every piece that ends at or before `cursor`.
    ///
    /// A piece a worker still holds is not memory the decoder can give back,
    /// so it is moved aside and goes on being charged until the worker is
    /// finished with it. Everything else is freed here, which is the whole
    /// point of the shape: input is let go as it is claimed, not compacted.
    pub(crate) fn retain_from(&mut self, cursor: u64) {
        while let Some(front) = self.q.front() {
            if front.end() > cursor {
                break;
            }
            let seg = self.q.pop_front().expect("front exists");
            self.base = seg.end();
            self.release(seg);
        }
        if let Some(front) = self.q.front()
            && cursor > front.start
        {
            self.base = cursor;
        }
        self.sweep();
    }

    /// Lets go of a piece the queue has finished with, or sets it aside if a
    /// worker has not.
    fn release(&mut self, seg: Seg) {
        if Arc::strong_count(&seg.data) > 1 {
            self.lent.push(seg);
            return;
        }
        self.give_back(seg);
    }

    /// Takes the charge off a piece nothing holds any more, and keeps its
    /// allocation if it is one of the decoder's own and the right size for
    /// what is arriving.
    fn give_back(&mut self, seg: Seg) {
        self.held -= seg.charge;
        if !seg.owned {
            return;
        }
        // The last holder of an allocation of the decoder's own: take the
        // buffer back out of its handles and park it if it is the size the
        // next piece will want.
        let Ok(data) = Arc::try_unwrap(seg.data) else {
            return;
        };
        let Ok(mut buf) = Arc::try_unwrap(data.data) else {
            return;
        };
        let cap = buf.capacity() as u64;
        if self.last_piece != 0 && cap > self.last_piece.saturating_mul(2) {
            return;
        }
        // One is always worth keeping - it is what recycling needs, and it is
        // what the next piece will ask for - and beyond that only while what
        // is parked stays inside the decoder's allowance.
        let first = self.spare.is_empty();
        if !first
            && (self.spare.len() >= self.park_slots || self.spare_bytes + cap > self.park_room)
        {
            return;
        }
        buf.clear();
        self.spare_bytes += cap;
        self.held += cap;
        self.spare.push(buf);
    }

    /// Gives back every set-aside piece whose last other holder has gone.
    pub(crate) fn sweep(&mut self) {
        let mut i = 0;
        while i < self.lent.len() {
            if Arc::strong_count(&self.lent[i].data) == 1 {
                let seg = self.lent.swap_remove(i);
                self.give_back(seg);
            } else {
                i += 1;
            }
        }
    }

    /// Forgets everything, for a decoder that has been cancelled.
    pub(crate) fn clear(&mut self) {
        self.q.clear();
        self.lent.clear();
        self.spare.clear();
        self.spare_bytes = 0;
        self.held = 0;
        self.base = self.end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(n: usize, byte: u8) -> Vec<u8> {
        alloc::vec![byte; n]
    }

    #[test]
    fn a_piece_is_charged_until_the_cursor_is_past_it() {
        let mut q = SegQueue::default();
        q.push_owned(piece(100, 1));
        q.push_owned(piece(100, 2));
        assert_eq!(q.held_bytes(), 200);
        assert_eq!(q.len(), 200);

        // Inside the first piece: nothing can be given back yet, because the
        // rest of it is still to be read.
        q.retain_from(50);
        assert_eq!(q.held_bytes(), 200);
        assert_eq!(q.dead_bytes(), 50);

        // Past the first piece: its bytes are no longer the stream's, but the
        // allocation is kept for the next piece, so it is still charged - and
        // it is not dead weight, because a copy will go into it.
        q.retain_from(100);
        assert_eq!(q.held_bytes(), 200);
        assert_eq!(q.spare_bytes(), 100);
        assert_eq!(q.dead_bytes(), 0);

        q.retain_from(200);
        assert_eq!(q.held_bytes(), 100);
        assert_eq!(q.spare_bytes(), 100);
        assert_eq!(q.len(), 0);

        // And it comes back emptied, with its capacity, for the next piece.
        let buf = q.take_spare().expect("parked");
        assert!(buf.is_empty());
        assert!(buf.capacity() >= 100);
        assert_eq!(q.held_bytes(), 0);
        assert!(q.take_spare().is_none());
    }

    #[test]
    fn what_is_parked_stays_inside_the_allowance() {
        let mut q = SegQueue::default();
        q.set_park_budget(250, 2);
        for i in 0..5u8 {
            q.push_owned(piece(100, i));
        }
        q.retain_from(500);
        // One is always kept; beyond that, two buffers and 250 bytes are all
        // the allowance permits, so the rest go back to the allocator.
        assert_eq!(q.spare_bytes(), 200);
        assert_eq!(q.held_bytes(), 200);

        // A buffer far larger than the pieces now arriving is not worth
        // keeping, whatever the allowance says.
        let mut q = SegQueue::default();
        q.set_park_budget(1 << 20, 4);
        q.push_owned(piece(4096, 1));
        q.push_owned(piece(100, 2));
        q.retain_from(4096 + 100);
        assert_eq!(q.spare_bytes(), 100);
    }

    #[test]
    fn a_lent_range_is_never_parked() {
        let buf = Arc::new(piece(100, 3));
        let mut q = SegQueue::default();
        q.set_park_budget(1 << 20, 4);
        q.push_shared(&buf, 0..100);
        q.retain_from(100);
        // The allocation was the caller's, so there is nothing to keep.
        assert_eq!(q.spare_bytes(), 0);
        assert_eq!(q.held_bytes(), 0);
        assert!(q.take_spare().is_none());
        assert_eq!(Arc::strong_count(&buf), 1);
    }

    #[test]
    fn a_claimed_piece_is_charged_once_and_only_until_its_holder_drops_it() {
        let mut q = SegQueue::default();
        q.push_owned(piece(100, 7));
        let claim = q.claim(0..100).expect("wholly held");
        // The claim is a second reference to bytes already charged, not a
        // second charge.
        assert_eq!(q.held_bytes(), 100);
        assert_eq!(claim.len(), 1);
        assert_eq!(claim[0].bytes(), &piece(100, 7)[..]);

        // Past the piece with the claim still out: the queue cannot give the
        // memory back, because the holder is still reading it.
        q.retain_from(100);
        assert_eq!(q.held_bytes(), 100);

        drop(claim);
        q.sweep();
        // Charged no longer for a piece of the stream, but for an allocation
        // kept to be filled again.
        assert_eq!(q.spare_bytes(), 100);
        assert_eq!(q.held_bytes(), 100);
        assert!(q.take_spare().is_some());
        assert_eq!(q.held_bytes(), 0);
    }

    #[test]
    fn a_run_spanning_pieces_is_claimed_as_the_pieces_it_spans() {
        let mut q = SegQueue::default();
        q.push_owned(piece(10, 1));
        q.push_owned(piece(10, 2));
        q.push_owned(piece(10, 3));
        let claim = q.claim(5..25).expect("wholly held");
        let got: Vec<u8> = claim.iter().flat_map(|s| s.bytes().to_vec()).collect();
        assert_eq!(got.len(), 20);
        assert_eq!(&got[..5], &[1; 5]);
        assert_eq!(&got[5..15], &[2; 10]);
        assert_eq!(&got[15..], &[3; 5]);
        // Half a run is never handed over.
        assert!(q.claim(20..40).is_none());
    }

    #[test]
    fn a_lent_piece_is_given_back_although_the_caller_still_holds_the_buffer() {
        let buf = Arc::new(piece(100, 9));
        let mut q = SegQueue::default();
        q.push_shared(&buf, 0..40);
        q.push_shared(&buf, 40..100);
        // What is charged is the bytes lent, not the allocation, and lending
        // twice out of one buffer charges each range once.
        assert_eq!(q.held_bytes(), 100);

        let claim = q.claim(0..40).expect("wholly held");
        q.retain_from(40);
        // The first range is out with a holder; the second is still to be read.
        assert_eq!(q.held_bytes(), 100);
        drop(claim);
        q.sweep();
        assert_eq!(q.held_bytes(), 60);

        q.retain_from(100);
        // Nothing of the queue's is left, though the caller's buffer lives on.
        assert_eq!(q.held_bytes(), 0);
        assert_eq!(Arc::strong_count(&buf), 1);
        assert_eq!(*buf, piece(100, 9));
    }

    #[test]
    fn the_piece_at_an_offset_is_the_rest_of_that_piece() {
        let mut q = SegQueue::default();
        q.push_owned(piece(10, 4));
        q.push_owned(piece(10, 5));
        assert_eq!(q.piece_at(0).expect("held").len(), 10);
        assert_eq!(q.piece_at(7).expect("held"), &[4; 3]);
        assert_eq!(q.piece_at(10).expect("held"), &[5; 10]);
        assert!(q.piece_at(20).is_none());
    }

    #[test]
    fn clearing_forgets_everything_and_charges_nothing() {
        let mut q = SegQueue::default();
        q.push_owned(piece(100, 1));
        q.clear();
        assert_eq!(q.held_bytes(), 0);
        assert_eq!(q.len(), 0);
        assert!(q.piece_at(0).is_none());
    }
}
