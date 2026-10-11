//! The threaded match finder.
//!
//! C: `C/LzFindMt.c`, `C/LzFindMt.h` and the `GetMatchesSpecN_2` kernel in
//! `C/LzFindOpt.c`.
//!
//! Three threads split the work the single-threaded [`MatchFinder`] does in
//! one:
//!
//! * the **hash** thread reads the stream and turns each position into a head
//!   distance for the high hash table, writing runs of them into `hash_buf`
//!   (C: `HashThreadFunc`, with the `GetHeads*` family);
//! * the **bt** thread consumes those heads, walks and rebalances the binary
//!   tree in `son`, and writes finished match lists into `bt_buf`
//!   (C: `BtThreadFunc` over `BtGetMatches` and `GetMatchesSpecN_2`);
//! * the **lz** thread is whoever called the encoder. It consumes `bt_buf`
//!   and mixes in the short matches from the low hash table
//!   (C: `MatchFinderMt_GetMatches` over `MixMatches2/3/4`).
//!
//! The matches this produces are the same matches the single-threaded finder
//! produces, so the encoder's output is bit-exact either way; that is what
//! `tests/lzma_parity.rs` checks.
//!
//! # How the threads share memory
//!
//! Everything the three threads touch lives in one [`MtInner`] behind an
//! [`UnsafeCell`], reached through raw pointers. That is the C's own shape and
//! the reason the `unsafe` here is what it is: `CMatchFinderMt` is one struct
//! that three threads write to at once, and it is correct because the
//! fields - and the *ranges* of the three tables - are partitioned between
//! them by the block protocol in [`MtSync`], not because anything is locked.
//!
//! The partition, which every `SAFETY` comment below refers back to:
//!
//! | region | written by | read by |
//! | --- | --- | --- |
//! | `win` below `stream_pos` | hash thread (appends) | bt, lz |
//! | `tab[..fixed_hash_size]` (low hash) | lz | lz |
//! | `tab[fixed_hash_size..son_base]` (high hash) | hash | hash |
//! | `tab[son_base..]` (`son`) | bt | bt |
//! | `hash_buf` block `i` | hash | bt |
//! | `bt_buf` block `i` | bt | lz |
//!
//! A block of `hash_buf` or `bt_buf` is handed over by
//! [`MtSync::get_next_block`]: the producer releases `filled` only after it
//! has finished writing the block, and the consumer releases `free` only after
//! it has finished reading it, so at most one thread is inside any block at a
//! time. The two `CriticalSection`s exist for the one thing that is *not*
//! partitioned - `MatchFinder_MoveBlock` slides the whole window - and the
//! hash thread takes both before it moves, which is the only moment all three
//! agree to stand still.
//!
//! The move does *not* write the bt and lz threads' window indices itself, as
//! `HashThreadFunc` does with `mt->buffer -= offset` and
//! `mt->pointerToCurPos -= offset`. The C can, because every one of its reads
//! of `p->buffer` goes through `p` and the critical-section calls are opaque
//! to the compiler. Here each owner works on its state through a `&mut`, which
//! promises the compiler that nothing else writes it - and it is held across
//! the very `get_next_block` call the move happens inside, so the optimizer is
//! entitled to keep the old index in a register and write it back afterwards.
//! It did: the bt thread carried on from its pre-move index, walked off the
//! end of the window and panicked. So the move publishes how far it slid in
//! [`MtShared::bt_shift`] and [`MtShared::lz_shift`], and each owner subtracts
//! its pending shift the moment it re-enters the critical section, which is
//! the first moment the C's own reasoning lets it look at the window again.
//!
//! # When a thread panics
//!
//! A panic on the hash or bt thread is caught at the block it happened in and
//! turned into the C's own failure protocol instead of a dead thread: the bt
//! thread marks `failure_BT` and hands the lz thread an empty block, which is
//! what `C/LzFindMt.c` does with corrupted tables; the hash thread hands the
//! bt thread a header no real block carries, which the bt thread turns into
//! the same `failure_BT`. It must not end the stream instead: the bt blocks
//! already published promise the lz thread every byte read so far, and a
//! short end of stream would leave it reading past the last entry the bt
//! thread wrote. Either way [`MtShared::thread_failed`] is raised and the encoder's
//! `CheckErrors` returns an error. Before this, a panic left the surviving
//! threads waiting on semaphores the dead one would never release.
//!
//! # Not ported
//!
//! `p->affinity` / `affinityGroup` (thread affinity, which `LzmaEnc` never
//! sets), the `MFMT_GM_INLINE`-off fallback loop in `BtGetMatches`, and the
//! commented-out `MatchFinderMt_GetMatches_Bt4` / `MatchFinderMt4_Skip` /
//! `BT5_USE_H4` variants.

use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::enc::consts::*;
use crate::enc::lz_find::MatchFinder;
use crate::enc::match_run::match_run;
use crate::enc::stream::SeqInStream;
use crate::error::Error;
use crate::mt::event::Event;
use crate::mt::sync::{CriticalSection, Semaphore};

/// C: `kMtHashBlockSize`.
const HASH_BLOCK_SIZE: u32 = 1 << 17;
/// C: `kMtHashNumBlocks`.
const HASH_NUM_BLOCKS: u32 = 1 << 1;
/// C: `kMtBtBlockSize`.
const BT_BLOCK_SIZE: u32 = 1 << 16;
/// C: `kMtBtNumBlocks`.
const BT_NUM_BLOCKS: u32 = 1 << 4;
/// C: `kHashBufferSize`.
const HASH_BUFFER_SIZE: usize = (HASH_BLOCK_SIZE * HASH_NUM_BLOCKS) as usize;
/// C: `kBtBufferSize`.
const BT_BUFFER_SIZE: usize = (BT_BLOCK_SIZE * BT_NUM_BLOCKS) as usize;
/// C: `kMtMaxValForNormalize`.
const MT_MAX_VAL_FOR_NORMALIZE: u32 = 0xFFFF_FFFF;
/// C: `BT_HASH_BYTES_MAX`.
const BT_HASH_BYTES_MAX: u32 = 5;

/// C: `GET_HASH_BLOCK_OFFSET`.
#[inline]
const fn hash_block_offset(i: u32) -> usize {
    ((i & (HASH_NUM_BLOCKS - 1)) * HASH_BLOCK_SIZE) as usize
}

/// C: `GET_BT_BLOCK_OFFSET`.
#[inline]
const fn bt_block_offset(i: u32) -> usize {
    ((i & (BT_NUM_BLOCKS - 1)) * BT_BLOCK_SIZE) as usize
}

// ---------------------------------------------------------------------------
// CMtSync
// ---------------------------------------------------------------------------

/// The handshake one producer thread has with its consumer.
///
/// C: `CMtSync`. `free` and `filled` bound the ring of blocks, `can_start` and
/// `was_stopped` start and stop a run, and `cs` is held by the *consumer*
/// across `get_next_block` calls so that the hash thread cannot slide the
/// window underneath it.
///
/// The `BoolInt` and `UInt32` flags the C reads without a lock are atomics
/// here. Each is touched once per block, and every one of them is already
/// ordered by a semaphore or an event, so the orderings are not load-bearing;
/// they are spelled `SeqCst` because nothing here is hot enough to be worth a
/// weaker argument.
pub(crate) struct MtSync {
    /// C: `p->numProcessedBlocks`.
    num_processed_blocks: AtomicU32,
    /// C: `p->needStart`.
    need_start: AtomicBool,
    /// C: `p->exit`.
    exit: AtomicBool,
    /// C: `p->stopWriting`.
    stop_writing: AtomicBool,
    /// C: `p->canStart`.
    can_start: Event,
    /// C: `p->wasStopped`.
    was_stopped: Event,
    /// C: `p->freeSemaphore`.
    free: Semaphore,
    /// C: `p->filledSemaphore`.
    filled: Semaphore,
    /// C: `p->cs`.
    cs: CriticalSection,
    /// C: `p->csWasEntered`.
    cs_was_entered: AtomicBool,
    /// C: `p->wasCreated`.
    was_created: AtomicBool,
}

impl MtSync {
    /// C: `MtSync_Construct`.
    fn new() -> Self {
        MtSync {
            num_processed_blocks: AtomicU32::new(0),
            need_start: AtomicBool::new(true),
            exit: AtomicBool::new(true),
            stop_writing: AtomicBool::new(false),
            can_start: Event::new(),
            was_stopped: Event::new(),
            free: Semaphore::new(),
            filled: Semaphore::new(),
            cs: CriticalSection::new(),
            cs_was_entered: AtomicBool::new(false),
            was_created: AtomicBool::new(false),
        }
    }

    fn get(f: &AtomicBool) -> bool {
        f.load(Ordering::SeqCst)
    }

    fn set(f: &AtomicBool, v: bool) {
        f.store(v, Ordering::SeqCst);
    }

    /// C: `LOCK_BUFFER`.
    fn lock_buffer(&self) {
        self.cs.enter();
        Self::set(&self.cs_was_entered, true);
    }

    /// C: `UNLOCK_BUFFER`.
    fn unlock_buffer(&self) {
        self.cs.leave();
        Self::set(&self.cs_was_entered, false);
    }

    /// C: `MtSync_Init`, "call it before each new file".
    fn init(&self, num_blocks: u32) -> Result<(), Error> {
        if !Self::get(&self.need_start) || Self::get(&self.cs_was_entered) {
            return Err(Error::InternalFailure);
        }
        self.free.init(num_blocks);
        self.filled.init(0);
        Ok(())
    }

    /// C: `MtSync_GetNextBlock`. Returns the index of the block that is now
    /// filled, and leaves the buffer *locked* for the caller - the next call
    /// is what unlocks it.
    fn get_next_block(&self) -> u32 {
        let mut num_blocks = 0;
        if Self::get(&self.need_start) {
            self.num_processed_blocks.store(1, Ordering::SeqCst);
            Self::set(&self.need_start, false);
            Self::set(&self.stop_writing, false);
            Self::set(&self.exit, false);
            self.was_stopped.reset();
            self.can_start.set();
        } else {
            self.unlock_buffer();
            // C: "we free current block".
            num_blocks = self.num_processed_blocks.fetch_add(1, Ordering::SeqCst);
            self.free.release1();
        }

        self.filled.wait();
        self.lock_buffer();
        num_blocks
    }

    /// C: `MtSync_StopWriting`.
    fn stop_writing(&self) {
        if !Self::get(&self.was_created) || Self::get(&self.need_start) {
            return;
        }
        if Self::get(&self.cs_was_entered) {
            self.unlock_buffer();
        }
        // C: "We send (p->stopWriting) message and release freeSemaphore to
        // free current block. So the thread will see (p->stopWriting) at some
        // iteration after Wait(freeSemaphore)."
        Self::set(&self.stop_writing, true);
        self.free.release1();
        self.was_stopped.wait();
        // C 21.03: "we don't restore semaphore counters here. We will recreate
        // and reinit semaphores in next start."
        Self::set(&self.need_start, true);
    }

    /// C: the `p->exit = True; Event_Set(&p->canStart);` half of
    /// `MtSync_Destruct`, which is how a stopped thread is told to return.
    fn send_exit(&self) {
        Self::set(&self.exit, true);
        self.can_start.set();
    }

    fn exiting(&self) -> bool {
        Self::get(&self.exit)
    }

    fn stopping(&self) -> bool {
        Self::get(&self.stop_writing)
    }
}

// ---------------------------------------------------------------------------
// The hash thread's kernel
// ---------------------------------------------------------------------------

/// Which `GetHeads*` the hash thread runs.
///
/// C: the `p->GetHeadsFunc` assignment in `MatchFinderMt_CreateVTable`. The
/// `b` variants are the `bigHash` ones, chosen when `hashMask >= 0xFFFFFF`;
/// `C/LzFind.c` guarantees that bound (see its "GetHeads4b() needs
/// (hs >= ((1 << 24) - 1))" comment) so the unmasked 24-bit value they mix in
/// always fits the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heads {
    H2,
    H3,
    H3b,
    H4,
    H4b,
    H5,
    H5b,
}

#[inline]
fn ui16(p: &[u8], i: usize) -> u32 {
    u32::from(p[i]) | (u32::from(p[i + 1]) << 8)
}

/// C: `GetUi24hi_from32(p)`, the top three bytes of a little-endian 32-bit
/// load.
#[inline]
fn ui24hi(p: &[u8], i: usize) -> u32 {
    u32::from(p[i + 1]) | (u32::from(p[i + 2]) << 8) | (u32::from(p[i + 3]) << 16)
}

/// C: the `GetHeads2` / `GetHeads3` / ... family over `GetHeads_LOOP`.
///
/// `heads[k]` becomes the distance from position `pos + k` back to the last
/// position with the same hash, and the table is updated to point at `pos + k`
/// — the same "insert and report the previous head" step the single-threaded
/// finder does inline, lifted out so that it can run a block ahead.
///
/// The C's `USE_GetHeads_LOCAL_CRC` builds per-call copies of the byte table
/// with `hashMask` already folded in. That is a strength reduction, not a
/// different value: every term it leaves unmasked is at most 24 bits and
/// `hashMask` always has its low 16 (`bigHash`: 24) bits set, so masking the
/// terms and masking the sum agree. The masks are written where the C puts
/// them so the two can be read against each other.
fn get_heads(
    kind: Heads,
    p: &[u8],
    mut pos: u32,
    hash: &mut [u32],
    mask: u32,
    heads: &mut [u32],
    crc: &[u32; 256],
) {
    for (i, head) in heads.iter_mut().enumerate() {
        let value = match kind {
            Heads::H2 => ui16(p, i),
            Heads::H3 => (crc[usize::from(p[i])] ^ ui16(p, i + 1)) & mask,
            Heads::H3b => ui16(p, i) ^ (u32::from(p[i + 2]) << 16),
            Heads::H4 => {
                (crc[usize::from(p[i])] & mask)
                    ^ ((crc[usize::from(p[i + 3])] << K_LZ_HASH_CRC_SHIFT_1) & mask)
                    ^ ui16(p, i + 1)
            }
            Heads::H4b => (crc[usize::from(p[i])] & mask) ^ ui24hi(p, i),
            Heads::H5 => {
                (crc[usize::from(p[i])] & mask)
                    ^ ((crc[usize::from(p[i + 3])] << K_LZ_HASH_CRC_SHIFT_1) & mask)
                    ^ ((crc[usize::from(p[i + 4])] << K_LZ_HASH_CRC_SHIFT_2) & mask)
                    ^ ui16(p, i + 1)
            }
            Heads::H5b => {
                (crc[usize::from(p[i])] & mask)
                    ^ ((crc[usize::from(p[i + 4])] << K_LZ_HASH_CRC_SHIFT_1) & mask)
                    ^ ui24hi(p, i)
            }
        } as usize;
        *head = pos.wrapping_sub(hash[value]);
        hash[value] = pos;
        pos = pos.wrapping_add(1);
    }
}

// ---------------------------------------------------------------------------
// The bt thread's kernel
// ---------------------------------------------------------------------------

/// `s[i]`, with the bounds check left to the caller.
///
/// # Safety
///
/// `i < s.len()`.
#[inline(always)]
unsafe fn at<T: Copy>(s: &[T], i: usize) -> T {
    debug_assert!(i < s.len());
    // SAFETY: the caller's contract.
    unsafe { *s.get_unchecked(i) }
}

/// `s[i] = v`, with the bounds check left to the caller.
///
/// # Safety
///
/// `i < s.len()`.
#[inline(always)]
unsafe fn put<T: Copy>(s: &mut [T], i: usize, v: T) {
    debug_assert!(i < s.len());
    // SAFETY: the caller's contract.
    unsafe { *s.get_unchecked_mut(i) = v }
}

/// C: `GetMatchesSpecN_2` in `C/LzFindOpt.c`.
///
/// The binary-tree walk, run over a whole run of positions instead of one.
/// `heads` is the run of head *distances* the hash thread produced; for each
/// one this descends the tree rooted at that distance, emits the match pairs
/// it finds, and rebalances `son` - the same work `GetMatchesSpec1` does for a
/// single position, with the hash lookup already done.
///
/// Returns the new cursor into `d` and the position reached (C: `*posRes`), or
/// [`None`] where the C returns `NULL`: a `son` entry that points forward, or
/// a head distance of zero, both of which mean the tables have been corrupted
/// and the caller must fail rather than read out of bounds.
///
/// `USE_SON_PREFETCH` is a load hoist with no effect on the values and is left
/// to the compiler. `USE_LONG_MATCH_OPT` is *not* - it changes what is emitted
/// (a run of `2`-pair entries) - and is ported.
///
/// # Bounds
///
/// The accesses a walk makes once per node - the node's two sons, the two
/// bytes at the current length, and the store that relinks the tree - are not
/// bounds-checked. Everything they rest on is established from the arguments
/// by the asserts at the top, on every call, so this is a safe function: with
/// any arguments and any contents of `win`, `son` and `heads`, a call that
/// could step outside a slice panics before its first unchecked access. The
/// accesses made once per position or less (`heads`, `d`, the empty node, the
/// full-length match and its long-match run) keep their checks, and nothing
/// here forms a hash index: the hash thread does, in `get_heads`, with a
/// masked value and a checked index.
///
/// Write `n` for `hsize - hi` at entry, the positions this call may consume,
/// and `k` for how many it has consumed. The asserts establish, of the values
/// at entry:
///
/// - **A** `cyclic_buffer_pos + n <= cyclic_buffer_size`
/// - **B** `2 * cyclic_buffer_size <= son.len()`
/// - **C** `cur + max_len_0 <= len_limit_0`
/// - **D** `len_limit_0 + n <= win.len()`
/// - **E** `min(pos, cyclic_buffer_size) <= cur + 1`
///
/// **Counters (K).** A head is taken before anything is touched, so `k >= 1`
/// in a walk, where `cur`, `pos` and `cyclic_buffer_pos` are their entry values
/// plus `k - 1` and `len_limit` is `len_limit_0 + k`. The long-match loop moves
/// all four on by one and then takes the next head, which it does only after
/// testing `hi != hsize`; so in a walk `k <= n`, and `cyclic_buffer_pos` stays
/// at most its entry value plus `n - 1`, which by A is below
/// `cyclic_buffer_size`.
///
/// **The tree.** Every node is visited with `1 <= delta < cbs`, where `cbs` is
/// `min(pos, cyclic_buffer_size)` taken when the walk began. The first `delta`
/// is a head: zero is refused, and one not below `cbs` takes the empty-node
/// branch instead of a walk. A later one is `pos - m` for a son `m` that was
/// tested `m < cur_match`, and `cur_match = pos - delta` is exact because
/// `delta < cbs <= pos`; so the new `delta` is at least one and no wrap, and
/// the walk goes round again only after testing it below `cbs`. From that,
/// `back` - `cyclic_buffer_pos - delta`, or `cbs - (delta - cyclic_buffer_pos)`
/// where that would be negative - is below `cyclic_buffer_size`: the first is
/// at most `cyclic_buffer_pos`, the second less than `cbs`. So `pair + 1 =
/// 2 * back + 1` is below `2 * cyclic_buffer_size`, inside `son` by B. `ptr0`
/// and `ptr1` begin as `2 * cyclic_buffer_pos + 1` and `2 * cyclic_buffer_pos`,
/// inside by K and B, and afterwards hold a `pair` or `pair + 1` of an earlier
/// node. Every tree index is thus formed from a distance already tested
/// against the cyclic size.
///
/// **The window.** `max_len` stays below `len_limit` through a walk: it
/// begins at `cur + max_len_0`, which C and K put at or below `len_limit - 1`,
/// and is raised only to a scan result, and a scan result equal to the limit
/// ends the walk. `len0` and `len1` begin at `cur`, below the limit for the
/// same reason, and are only ever set to a `len` at which bytes were read. At
/// the top of a node, then, `cur <= len < len_limit`. The scan is asked for
/// `len + 1 ..= len_limit`, and its answer is tested to lie there rather than
/// trusted, so `len` only grows; if the answer is the limit, `max_len < len`
/// and the walk ends without another read, and otherwise `len < len_limit`
/// again. So both byte reads at the current length are at
/// `len <= len_limit - 1`, and `len_limit <= len_limit_0 + n` by K, which D
/// puts inside the window. The
/// older byte is at `len - diff` with `diff = delta <= cbs - 1`; E and K give
/// `min(pos, cyclic_buffer_size) <= cur + 1` at every position (both sides
/// move on together, and a `pos` that wraps only makes the left side
/// smaller), so `diff <= cur <= len` and the index neither underflows nor
/// exceeds `len`.
///
/// **What changes under the caller.** None of the above depends on where the
/// stream is or on what the tables hold. The window slide and the position
/// normalisation both happen between calls, and the five conditions are
/// tested again on the next: either can at worst make a call panic, which the
/// bt thread reports as a failed stream. They do not, for the reasons given
/// where the kernel is called. The slices themselves stay valid for the call:
/// `son` is the bt thread's alone, and the hash thread, which refills the
/// window while this runs, writes only above the stream position it had
/// published when it produced these heads and moves the window only under
/// `hash_sync.cs`. The bt thread holds that across every call of this
/// function: it lets go only inside `get_next_block`, between calls, and takes
/// up any move before it cuts the window slice for the next. Every byte read
/// here is below `len_limit_0 + n`, which the caller makes
/// `b.buffer + hash_num_avail` at most - that published position - so the
/// producer never writes a byte this call reads.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn get_matches_spec_n_2(
    win: &[u8],
    len_limit_0: usize,
    mut pos: u32,
    mut cur: usize,
    son: &mut [u32],
    cut_value: u32,
    d: &mut [u32],
    mut di: usize,
    max_len_0: usize,
    heads: &[u32],
    mut hi: usize,
    limit: usize,
    hsize: usize,
    mut cyclic_buffer_pos: u32,
    cyclic_buffer_size: u32,
) -> Option<(usize, u32)> {
    if hi == hsize {
        return Some((di, pos));
    }
    // The conditions of "# Bounds". They hold on every call the bt thread
    // makes; a caller that broke one would otherwise reach the unchecked
    // accesses below with indices nothing vouches for.
    assert!(hi < hsize && hsize <= heads.len(), "heads");
    let n = hsize - hi;
    let cyclic = cyclic_buffer_size as usize;
    assert!(
        (cyclic_buffer_pos as usize)
            .checked_add(n)
            .is_some_and(|end| end <= cyclic),
        "A: the run passes the end of the cyclic buffer"
    );
    assert!(son.len() / 2 >= cyclic, "B: the tree is short");
    assert!(
        cur.checked_add(max_len_0)
            .is_some_and(|end| end <= len_limit_0),
        "C: no room for the hashed bytes below the limit"
    );
    assert!(
        len_limit_0
            .checked_add(n)
            .is_some_and(|end| end <= win.len()),
        "D: the run passes the end of the window"
    );
    assert!(
        pos.min(cyclic_buffer_size) as usize <= cur + 1,
        "E: a distance could reach before the window"
    );

    let mut len_limit = len_limit_0;
    loop {
        if hi == hsize {
            break;
        }
        let mut delta = heads[hi];
        hi += 1;
        if delta == 0 {
            return None;
        }
        len_limit += 1;

        let mut cbs = cyclic_buffer_size;
        if pos < cbs {
            if delta > pos {
                return None;
            }
            cbs = pos;
        }

        if delta >= cbs {
            let ptr1 = (cyclic_buffer_pos as usize) << 1;
            d[di] = 0;
            di += 1;
            son[ptr1] = K_EMPTY_HASH_VALUE;
            son[ptr1 + 1] = K_EMPTY_HASH_VALUE;
        } else {
            di += 1;
            let distances = di;

            let mut ptr0 = ((cyclic_buffer_pos as usize) << 1) + 1;
            let mut ptr1 = (cyclic_buffer_pos as usize) << 1;

            let mut cut_value = cut_value;
            let (mut len0, mut len1) = (cur, cur);
            let mut max_len = cur + max_len_0;
            // K: the slot being linked is inside the cyclic buffer.
            debug_assert!((cyclic_buffer_pos as usize) < cyclic);
            debug_assert!(cbs == pos.min(cyclic_buffer_size));
            debug_assert!(len_limit <= len_limit_0 + n && len_limit <= win.len());

            loop {
                debug_assert!(delta != 0 && delta < cbs && cbs <= cyclic_buffer_size);
                // C: the "SPEC code" wrap, `_cyclicBufferPos - delta
                // (+ cbs if it went below zero)`, written so that neither arm
                // leaves `u32` on the way: `delta` is below `cbs`.
                let back = if cyclic_buffer_pos < delta {
                    cbs - (delta - cyclic_buffer_pos)
                } else {
                    cyclic_buffer_pos - delta
                };
                debug_assert!((back as usize) < cyclic);
                let pair = (back as usize) << 1;

                let diff = delta as usize;
                let mut len = if len0 < len1 { len0 } else { len1 };

                // SAFETY: "the tree" in `# Bounds`. `delta` was tested below
                // `cbs`, which is at most the cyclic size, before this node
                // was reached, so `back` is below the cyclic size and
                // `pair = 2 * back` is below `2 * cyclic <= son.len()`
                // (assert B).
                let pair0 = unsafe { at(son, pair) };

                // C: `if (len[diff] == len[0])`, the one byte the C tests
                // before it scans. Most nodes of a walk differ right here, so
                // asking this first keeps the word scan, and the two slices it
                // cuts, off the common path; the bytes loaded for it are the
                // ones the ordering test below needs, so a node that differs
                // costs two loads and no more. The walk never reaches a node
                // with `len` at the limit: both bounds start at `cur`, below
                // it, and a scan that runs to the limit ends the walk.
                debug_assert!(cur <= len && len < len_limit);
                debug_assert!(max_len < len_limit);
                debug_assert!(diff <= cur);
                // SAFETY: "the window" in `# Bounds`. `diff <= cur <= len`
                // (assert E carried along by the counters), so `len - diff`
                // does not underflow, and it is at most `len`, which the
                // next comment puts inside the window.
                let mut a = unsafe { at(win, len - diff) };
                // SAFETY: "the window" in `# Bounds`. `len < len_limit` at
                // the top of every node, and `len_limit <= len_limit_0 + n
                // <= win.len()` (the counters, assert D).
                let mut b = unsafe { at(win, len) };
                if a == b {
                    // The same scan `GetMatchesSpec1` runs, in `LzFindOpt.c`'s
                    // absolute window indices, from the byte after the one
                    // just tested.
                    let from = len;
                    len = match_run(win, diff, from + 1, len_limit);
                    // The scan answers within `from + 1 ..= len_limit`. The
                    // byte reads further down rest on that, so it is tested
                    // here and not taken on trust from another module.
                    if len <= from || len > len_limit {
                        return None;
                    }
                    if max_len < len {
                        max_len = len;
                        d[di] = (len - cur) as u32;
                        d[di + 1] = delta - 1;
                        di += 2;

                        if len == len_limit {
                            let pair1 = son[pair + 1];
                            son[ptr1] = pair0;
                            son[ptr0] = pair1;
                            d[distances - 1] = (di - distances) as u32;

                            // C: `USE_LONG_MATCH_OPT`. While the next position
                            // has the same head distance and the bytes at the
                            // far end still agree, the match simply shifts by
                            // one: emit it and copy the tree node instead of
                            // walking again.
                            if hi == hsize
                                || heads[hi] != delta
                                || win[len_limit - diff] != win[len_limit]
                                || di >= limit
                            {
                                break;
                            }
                            loop {
                                d[di] = 2;
                                d[di + 1] = (len_limit - cur) as u32;
                                d[di + 2] = delta - 1;
                                di += 3;
                                cur += 1;
                                len_limit += 1;
                                cyclic_buffer_pos += 1;
                                {
                                    let dest = (cyclic_buffer_pos as usize) << 1;
                                    let back = if cyclic_buffer_pos < delta {
                                        cbs - (delta - cyclic_buffer_pos)
                                    } else {
                                        cyclic_buffer_pos - delta
                                    };
                                    let src = (back as usize) << 1;
                                    let p0 = son[src];
                                    let p1 = son[src + 1];
                                    son[dest] = p0;
                                    son[dest + 1] = p1;
                                }
                                pos = pos.wrapping_add(1);
                                hi += 1;
                                if hi == hsize
                                    || heads[hi] != delta
                                    || win[len_limit - diff] != win[len_limit]
                                    || di >= limit
                                {
                                    break;
                                }
                            }
                            break;
                        }
                    }
                    // The scan stopped short of the limit, at the first byte
                    // that differs: that pair is what orders this node.
                    debug_assert!(diff < len && len < len_limit);
                    // SAFETY: "the window" in `# Bounds`. `len` is the scan's
                    // answer, tested above the `len` it started from, which
                    // was at least `diff`.
                    a = unsafe { at(win, len - diff) };
                    // SAFETY: "the window" in `# Bounds`. The answer was
                    // tested to be at most `len_limit`, and one equal to it
                    // left the walk above because `max_len < len_limit`; so
                    // `len < len_limit <= win.len()`.
                    b = unsafe { at(win, len) };
                }
                {
                    let cur_match = pos.wrapping_sub(delta);
                    debug_assert!(ptr0 < 2 * cyclic && ptr1 < 2 * cyclic);
                    if a < b {
                        // SAFETY: "the tree" in `# Bounds`. `back` is below
                        // the cyclic size, so `pair + 1 = 2 * back + 1` is
                        // below `2 * cyclic <= son.len()` (assert B).
                        delta = unsafe { at(son, pair + 1) };
                        // SAFETY: "the tree" in `# Bounds`. `ptr1` is
                        // `2 * cyclic_buffer_pos`, with `cyclic_buffer_pos`
                        // below the cyclic size (assert A and the counters),
                        // or the `pair + 1` of an earlier node; both are
                        // below `2 * cyclic <= son.len()`.
                        unsafe { put(son, ptr1, cur_match) };
                        ptr1 = pair + 1;
                        len1 = len;
                        if delta >= cur_match {
                            return None;
                        }
                    } else {
                        // `son[pair]`, loaded above; nothing has written the
                        // tree since.
                        delta = pair0;
                        // SAFETY: "the tree" in `# Bounds`. `ptr0` is
                        // `2 * cyclic_buffer_pos + 1`, with
                        // `cyclic_buffer_pos` below the cyclic size (assert A
                        // and the counters), or the `pair` of an earlier
                        // node; both are below `2 * cyclic <= son.len()`.
                        unsafe { put(son, ptr0, cur_match) };
                        ptr0 = pair;
                        len0 = len;
                        if delta >= cur_match {
                            return None;
                        }
                    }
                    delta = pos.wrapping_sub(delta);

                    cut_value -= 1;
                    if cut_value == 0 || delta >= cbs {
                        son[ptr0] = K_EMPTY_HASH_VALUE;
                        son[ptr1] = K_EMPTY_HASH_VALUE;
                        d[distances - 1] = (di - distances) as u32;
                        break;
                    }
                }
            }
        }
        pos = pos.wrapping_add(1);
        cyclic_buffer_pos += 1;
        cur += 1;
        if di >= limit {
            break;
        }
    }
    Some((di, pos))
}

/// [`get_matches_spec_n_2`] as it was with every access bounds-checked: the
/// reference the unchecked walk is compared against.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn get_matches_spec_n_2_checked(
    win: &[u8],
    len_limit_0: usize,
    mut pos: u32,
    mut cur: usize,
    son: &mut [u32],
    cut_value: u32,
    d: &mut [u32],
    mut di: usize,
    max_len_0: usize,
    heads: &[u32],
    mut hi: usize,
    limit: usize,
    hsize: usize,
    mut cyclic_buffer_pos: u32,
    cyclic_buffer_size: u32,
) -> Option<(usize, u32)> {
    let mut len_limit = len_limit_0;
    loop {
        if hi == hsize {
            break;
        }
        let mut delta = heads[hi];
        hi += 1;
        if delta == 0 {
            return None;
        }
        len_limit += 1;

        let mut cbs = cyclic_buffer_size;
        if pos < cbs {
            if delta > pos {
                return None;
            }
            cbs = pos;
        }

        if delta >= cbs {
            let ptr1 = (cyclic_buffer_pos as usize) << 1;
            d[di] = 0;
            di += 1;
            son[ptr1] = K_EMPTY_HASH_VALUE;
            son[ptr1 + 1] = K_EMPTY_HASH_VALUE;
        } else {
            di += 1;
            let distances = di;

            let mut ptr0 = ((cyclic_buffer_pos as usize) << 1) + 1;
            let mut ptr1 = (cyclic_buffer_pos as usize) << 1;

            let mut cut_value = cut_value;
            let (mut len0, mut len1) = (cur, cur);
            let mut max_len = cur + max_len_0;

            loop {
                // C: the "SPEC code" wrap, `_cyclicBufferPos - delta
                // (+ cbs if it went below zero)`.
                let back = if cyclic_buffer_pos < delta {
                    cyclic_buffer_pos + cbs - delta
                } else {
                    cyclic_buffer_pos - delta
                };
                let pair = (back as usize) << 1;

                let diff = delta as usize;
                let mut len = if len0 < len1 { len0 } else { len1 };

                let pair0 = son[pair];

                // C: `if (len[diff] == len[0])`, the one byte the C tests
                // before it scans. Most nodes of a walk differ right here, so
                // asking this first keeps the word scan, and the two slices it
                // cuts, off the common path; the bytes loaded for it are the
                // ones the ordering test below needs, so a node that differs
                // costs two loads and no more. The walk never reaches a node
                // with `len` at the limit: both bounds start at `cur`, below
                // it, and a scan that runs to the limit ends the walk.
                debug_assert!(len < len_limit);
                let mut a = win[len - diff];
                let mut b = win[len];
                if a == b {
                    // The same scan `GetMatchesSpec1` runs, in `LzFindOpt.c`'s
                    // absolute window indices, from the byte after the one
                    // just tested.
                    len = match_run(win, diff, len + 1, len_limit);
                    if max_len < len {
                        max_len = len;
                        d[di] = (len - cur) as u32;
                        d[di + 1] = delta - 1;
                        di += 2;

                        if len == len_limit {
                            let pair1 = son[pair + 1];
                            son[ptr1] = pair0;
                            son[ptr0] = pair1;
                            d[distances - 1] = (di - distances) as u32;

                            // C: `USE_LONG_MATCH_OPT`. While the next position
                            // has the same head distance and the bytes at the
                            // far end still agree, the match simply shifts by
                            // one: emit it and copy the tree node instead of
                            // walking again.
                            if hi == hsize
                                || heads[hi] != delta
                                || win[len_limit - diff] != win[len_limit]
                                || di >= limit
                            {
                                break;
                            }
                            loop {
                                d[di] = 2;
                                d[di + 1] = (len_limit - cur) as u32;
                                d[di + 2] = delta - 1;
                                di += 3;
                                cur += 1;
                                len_limit += 1;
                                cyclic_buffer_pos += 1;
                                {
                                    let dest = (cyclic_buffer_pos as usize) << 1;
                                    let back = if cyclic_buffer_pos < delta {
                                        cyclic_buffer_pos + cbs - delta
                                    } else {
                                        cyclic_buffer_pos - delta
                                    };
                                    let src = (back as usize) << 1;
                                    let p0 = son[src];
                                    let p1 = son[src + 1];
                                    son[dest] = p0;
                                    son[dest + 1] = p1;
                                }
                                pos = pos.wrapping_add(1);
                                hi += 1;
                                if hi == hsize
                                    || heads[hi] != delta
                                    || win[len_limit - diff] != win[len_limit]
                                    || di >= limit
                                {
                                    break;
                                }
                            }
                            break;
                        }
                    }
                    // The scan stopped short of the limit, at the first byte
                    // that differs: that pair is what orders this node.
                    a = win[len - diff];
                    b = win[len];
                }
                {
                    let cur_match = pos.wrapping_sub(delta);
                    if a < b {
                        delta = son[pair + 1];
                        son[ptr1] = cur_match;
                        ptr1 = pair + 1;
                        len1 = len;
                        if delta >= cur_match {
                            return None;
                        }
                    } else {
                        // `son[pair]`, loaded above; nothing has written the
                        // tree since.
                        delta = pair0;
                        son[ptr0] = cur_match;
                        ptr0 = pair;
                        len0 = len;
                        if delta >= cur_match {
                            return None;
                        }
                    }
                    delta = pos.wrapping_sub(delta);

                    cut_value -= 1;
                    if cut_value == 0 || delta >= cbs {
                        son[ptr0] = K_EMPTY_HASH_VALUE;
                        son[ptr1] = K_EMPTY_HASH_VALUE;
                        d[distances - 1] = (di - distances) as u32;
                        break;
                    }
                }
            }
        }
        pos = pos.wrapping_add(1);
        cyclic_buffer_pos += 1;
        cur += 1;
        if di >= limit {
            break;
        }
    }
    Some((di, pos))
}

// ---------------------------------------------------------------------------
// The shared state
// ---------------------------------------------------------------------------

/// Which `MixMatches*` the lz thread runs, and therefore which `Skip`.
///
/// C: the `p->MixMatchesFunc` / `vTable->Skip` pair in
/// `MatchFinderMt_CreateVTable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mix {
    /// C: `numHashBytes == 2`: no low hash at all, `MatchFinderMt0_Skip` and
    /// `MatchFinderMt2_GetMatches`.
    None,
    /// C: `MixMatches2` with `MatchFinderMt2_Skip`.
    Two,
    /// C: `MixMatches3` with `MatchFinderMt3_Skip`.
    Three,
    /// C: `MixMatches4` with `MatchFinderMt3_Skip`.
    Four,
}

/// Everything fixed at create time, plus the three allocations.
///
/// C: the `CMatchFinderMt` fields that `MatchFinderMt_Init` copies out of the
/// `CMatchFinder` once and no thread writes again, and the buffers themselves.
struct Common {
    /// C: `mf->bufBase`, the window. With `direct` set it is the block
    /// [`run_block`] is coding, which nothing writes through.
    win: *mut u8,
    /// C: `mf->blockSize`, or the block's length with `direct` set.
    win_len: usize,
    /// C: `mf->directInput`: the window is the input itself, all of it there
    /// from the start, and is never moved or read into.
    direct: bool,
    /// C: `mf->hash`: the low hash, then the high hash, then `son`.
    tab: *mut u32,
    /// C: `p->hashBuf`, with `p->btBuf` following it in the same allocation
    /// exactly as `MatchFinderMt_Create` lays it out, and two more words after
    /// that for `p->failureBuf`. Inline, one block of each.
    bufs: *mut u32,
    /// Where `btBuf` starts in `bufs`, in words: `HASH_BUFFER_SIZE`, or
    /// `HASH_BLOCK_SIZE` inline.
    bt_base: usize,
    /// Where the two `failureBuf` words start in `bufs`.
    park_base: usize,
    /// No threads: the caller's thread runs the hash and bt stages a block
    /// at a time when it needs the next block of matches. See
    /// [`MatchFinderMt::get_next_block_bt`].
    inline: bool,

    /// C: `mf->hashMask`.
    hash_mask: u32,
    /// C: `p->fixedHashSize`.
    fixed_hash_size: usize,
    /// Where `son` starts in `tab`.
    son_base: usize,
    /// C: the length of `son`, `cyclicBufferSize * 2` for a binary tree.
    son_len: usize,
    /// C: `p->historySize`.
    history_size: u32,
    /// C: `p->numHashBytes`.
    num_hash_bytes: u32,
    /// C: `p->matchMaxLen`.
    match_max_len: u32,
    /// C: `p->cutValue`.
    cut_value: u32,
    /// C: `p->cyclicBufferSize`.
    cyclic_buffer_size: u32,
    /// C: `mf->keepSizeBefore`.
    keep_size_before: u32,
    /// C: `mf->keepSizeAfter`.
    keep_size_after: u32,
    /// C: `p->crc`.
    crc: [u32; 256],
    /// C: `p->GetHeadsFunc`.
    heads: Heads,
    /// C: `p->MixMatchesFunc`.
    mix: Mix,
}

impl Common {
    /// No buffers: every length zero and every pointer dangling, so that a
    /// slice formed from them is empty rather than invalid.
    fn empty() -> Self {
        Common {
            win: core::ptr::NonNull::dangling().as_ptr(),
            win_len: 0,
            direct: false,
            tab: core::ptr::NonNull::dangling().as_ptr(),
            bufs: core::ptr::NonNull::dangling().as_ptr(),
            bt_base: 0,
            park_base: 0,
            inline: false,
            hash_mask: 0,
            fixed_hash_size: 0,
            son_base: 0,
            son_len: 0,
            history_size: 0,
            num_hash_bytes: 0,
            match_max_len: 0,
            cut_value: 0,
            cyclic_buffer_size: 0,
            keep_size_before: 0,
            keep_size_after: 0,
            crc: [0; 256],
            heads: Heads::H4,
            mix: Mix::Three,
        }
    }

    /// The window.
    ///
    /// # Safety
    ///
    /// The caller must be a thread the block protocol allows to read the
    /// window: the bt and lz threads always may (they read strictly below
    /// `stream_pos`, and the hash thread only appends above it or moves the
    /// whole window with both critical sections held), and the hash thread may
    /// when it is not itself writing.
    unsafe fn win(&self) -> &[u8] {
        // SAFETY: `win` points at `win_len` initialised bytes for as long as
        // the owning `MtShared` lives, which outlives every thread that holds
        // a clone of the `Arc`.
        unsafe { core::slice::from_raw_parts(self.win, self.win_len) }
    }

    /// The low hash table, which only the lz thread touches.
    ///
    /// # Safety
    ///
    /// Caller must be the lz thread (C: `MixMatches*` and
    /// `MatchFinderMt2/3_Skip`, all of which run there).
    #[allow(clippy::mut_from_ref)]
    unsafe fn low_hash(&self) -> &mut [u32] {
        // SAFETY: `tab[..fixed_hash_size]` is the lz thread's exclusive range
        // in the partition documented at the top of this module; no other
        // thread forms a reference into it.
        unsafe { core::slice::from_raw_parts_mut(self.tab, self.fixed_hash_size) }
    }

    /// The high hash table, which only the hash thread touches.
    ///
    /// # Safety
    ///
    /// Caller must be the hash thread (C: `MatchFinder_Init_HighHash` and
    /// `GetHeadsFunc`, both called from `HashThreadFunc`).
    #[allow(clippy::mut_from_ref)]
    unsafe fn high_hash(&self) -> &mut [u32] {
        // SAFETY: `tab[fixed_hash_size..son_base]` is the hash thread's
        // exclusive range; the lz thread stays below `fixed_hash_size` and the
        // bt thread stays at or above `son_base`.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.tab.add(self.fixed_hash_size),
                self.son_base - self.fixed_hash_size,
            )
        }
    }

    /// The `son` table, which only the bt thread touches.
    ///
    /// # Safety
    ///
    /// Caller must be the bt thread (C: `GetMatchesSpecN_2` and
    /// `MatchFinder_Normalize3` in `BtGetMatches`).
    #[allow(clippy::mut_from_ref)]
    unsafe fn son(&self) -> &mut [u32] {
        // SAFETY: `tab[son_base..]` is the bt thread's exclusive range.
        unsafe { core::slice::from_raw_parts_mut(self.tab.add(self.son_base), self.son_len) }
    }

    /// One block of `hash_buf`.
    ///
    /// # Safety
    ///
    /// Caller must hold that block: the hash thread after
    /// `Semaphore_Wait(free)` and before `Semaphore_Release1(filled)`, or the
    /// bt thread between the matching pair.
    #[allow(clippy::mut_from_ref)]
    unsafe fn hash_block(&self, offset: usize) -> &mut [u32] {
        // SAFETY: the semaphore pair in `MtSync` lets exactly one thread be
        // inside a given block at a time, and `offset` names a whole block.
        unsafe { core::slice::from_raw_parts_mut(self.bufs.add(offset), HASH_BLOCK_SIZE as usize) }
    }

    /// One block of `bt_buf`.
    ///
    /// Never the whole buffer: a reference to it would cover the blocks the
    /// other thread is writing at the same time, and forming one is itself
    /// an access to all of it.
    ///
    /// # Safety
    ///
    /// As [`Common::hash_block`], for the bt and lz threads.
    #[allow(clippy::mut_from_ref)]
    unsafe fn bt_block(&self, offset: usize) -> &mut [u32] {
        // SAFETY: as `hash_block`; `offset` names a whole block of `bt_buf`,
        // which starts `bt_base` words into `bufs`.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.bufs.add(self.bt_base + offset),
                BT_BLOCK_SIZE as usize,
            )
        }
    }

    /// The two words past `bt_buf` the lz thread parks on after a failure,
    /// at `BT_BUFFER_SIZE` in `bt_buf`'s indexing.
    ///
    /// # Safety
    ///
    /// Caller must be the lz thread, the only one that touches them.
    #[allow(clippy::mut_from_ref)]
    unsafe fn bt_park(&self) -> &mut [u32] {
        // SAFETY: `bufs` is `park_base + 2` words long
        // (`MatchFinderMt::create`), and no other thread forms a reference
        // to the last two.
        unsafe { core::slice::from_raw_parts_mut(self.bufs.add(self.park_base), 2) }
    }

    /// The block of `bt_buf` the lz thread is reading at `pos`, in
    /// `bt_buf`'s indexing, with that block's first index: a whole block, or
    /// the two parking words once `pos` has been sent there.
    ///
    /// # Safety
    ///
    /// Caller must be the lz thread, holding the block `pos` is in.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    unsafe fn bt_held(&self, pos: usize) -> (usize, &mut [u32]) {
        // Inline there is one block, so `pos` is either in it or parked.
        if pos >= BT_BUFFER_SIZE {
            // SAFETY: the caller is the lz thread.
            (BT_BUFFER_SIZE, unsafe { self.bt_park() })
        } else {
            let base = pos & !(BT_BLOCK_SIZE as usize - 1);
            // SAFETY: the caller holds the block `pos` is in.
            (base, unsafe { self.bt_block(base) })
        }
    }

    /// The whole `bt_buf` allocation, including the two parking words, for
    /// a test that fills it before any thread is started.
    ///
    /// # Safety
    ///
    /// No producer thread may be running.
    #[cfg(test)]
    #[allow(clippy::mut_from_ref)]
    unsafe fn bt_all(&self) -> &mut [u32] {
        // SAFETY: nothing else is running.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.bufs.add(self.bt_base),
                self.park_base + 2 - self.bt_base,
            )
        }
    }
}

/// C: the `CMatchFinder` window fields the hash thread owns.
struct HashState {
    /// C: `mf->pos`.
    pos: u32,
    /// C: `mf->buffer`, as an index into the window.
    buffer: usize,
    /// C: `mf->streamPos`.
    stream_pos: u32,
    /// C: `mf->streamEndWasReached`.
    stream_end_was_reached: bool,
    /// C: `mf->result`, as the hash thread itself reads it. The lz thread
    /// reads the copy in [`MtShared::read_result`] instead: on a pair kept
    /// for blocks it asks while this thread may hold `&mut HashState`.
    result: Result<(), Error>,
    /// A panic was caught on this thread: every block from here on is the
    /// end-of-stream header. Not in the C, which has no unwinding.
    failed: bool,
}

impl HashState {
    /// C: `GET_AVAIL_BYTES`.
    #[inline]
    fn avail(&self) -> u32 {
        self.stream_pos.wrapping_sub(self.pos)
    }
}

/// C: the `CMatchFinderMt` fields between `btSync` and `hashSync`, which the
/// bt thread owns.
struct BtState {
    /// C: `p->hashBufPos`.
    hash_buf_pos: u32,
    /// C: `p->hashBufPosLimit`.
    hash_buf_pos_limit: u32,
    /// C: `p->hashNumAvail`.
    hash_num_avail: u32,
    /// C: `p->failure_BT`.
    failure: bool,
    /// C: `p->pos`.
    pos: u32,
    /// C: `p->buffer`, as an index into the window.
    buffer: usize,
    /// C: `p->cyclicBufferPos`.
    cyclic_buffer_pos: u32,
}

/// C: the `CMatchFinderMt` fields above `btSync`, which the lz thread owns.
struct LzState {
    /// C: `p->pointerToCurPos`, as an index into the window.
    pointer_to_cur_pos: usize,
    /// C: `p->btBufPos`, as an index into `bt_buf` (see `Common::bt_held`).
    bt_buf_pos: usize,
    /// C: `p->btBufPosLimit`.
    bt_buf_pos_limit: usize,
    /// C: `p->lzPos`.
    lz_pos: u32,
    /// C: `p->btNumAvailBytes`.
    bt_num_avail_bytes: u32,
    /// C: `p->failure_LZ_BT`.
    failure_lz_bt: bool,
}

/// The whole of `CMatchFinderMt`, as the three threads divide it.
///
/// Each `UnsafeCell` is entered by exactly one thread, except during
/// `move_block`, which is the one operation that touches all three and takes
/// both critical sections first.
pub(crate) struct MtShared {
    /// Written only by the lz thread in [`MatchFinderMt::create`], between
    /// streams, when neither producer thread is past `can_start`; read by
    /// every thread through [`MtShared::common`] during a stream.
    common: UnsafeCell<Common>,
    /// The block a pair of threads kept across blocks reads its input from;
    /// see [`run_block`]. The hash thread's during a stream, the lz thread's
    /// between streams.
    block: UnsafeCell<BlockInput>,
    hash_state: UnsafeCell<HashState>,
    bt_state: UnsafeCell<BtState>,
    lz_state: UnsafeCell<LzState>,
    /// C: `p->hashSync`.
    hash_sync: MtSync,
    /// C: `p->btSync`.
    bt_sync: MtSync,
    /// How far the window has slid under the bt thread since it last held
    /// `hash_sync.cs`. C: the `mt->buffer -= offset` in `HashThreadFunc`,
    /// deferred to the owner; see the module docs.
    bt_shift: AtomicUsize,
    /// The same for the lz thread's `pointer_to_cur_pos` and `bt_sync.cs`.
    /// C: `mt->pointerToCurPos -= offset`.
    lz_shift: AtomicUsize,
    /// Raised when the hash or bt thread caught a panic. The lz thread reads
    /// it in `CheckErrors`, as the C reads `failure_LZ_BT`.
    thread_failed: AtomicBool,
    /// C: `mf->result`, published by the hash thread whenever it records an
    /// error and read by the lz thread in `CheckErrors`. Kept outside
    /// `hash_state` so that the read never overlaps the hash thread's borrow
    /// of that cell; the lock orders the write before the read.
    read_result: Mutex<Result<(), Error>>,
    /// The allocations `Common`'s pointers address. Nothing reads or writes
    /// through this cell while a thread can reach the pointers; it is here so
    /// that the buffers outlive every thread, and so that
    /// [`MatchFinderMt::create`] can take them back for the next block. It is
    /// entered only where `common` is written.
    own: UnsafeCell<Owned>,
}

/// The allocations behind [`Common`]'s pointers.
#[derive(Default)]
struct Owned {
    win: Vec<u8>,
    tab: Vec<u32>,
    bufs: Vec<u32>,
}

/// The block [`run_block`] hands the hash thread, as the C's
/// `MatchFinder_SET_DIRECT_INPUT_BUF` hands it `src`: what it reads into the
/// window in place of a stream.
struct BlockInput {
    ptr: *const u8,
    len: usize,
    /// How much of it the hash thread has read.
    pos: usize,
}

// SAFETY: `Common`'s three raw pointers address allocations owned by this same
// struct, so they are valid for as long as any thread can reach them, and
// `BlockInput`'s addresses a block that `run_block` keeps borrowed until both
// producer threads have stopped. Every other field is either an atomic or an
// `UnsafeCell` whose contents are partitioned between the threads by the block
// protocol described at the top of this module - `common`, `own` and `block`
// by the start and stop of a stream - the partition, not a lock, is what makes
// the concurrent access disjoint, which is exactly the argument
// `C/LzFindMt.c` makes for the same struct.
unsafe impl Send for MtShared {}
// SAFETY: as above.
unsafe impl Sync for MtShared {}

impl MtShared {
    /// The settings and buffers every thread reads during a stream.
    #[inline]
    fn common(&self) -> &Common {
        // SAFETY: `common` is written only in `MatchFinderMt::create`, on the
        // lz thread, between streams: the producer threads are either joined
        // or waiting on `can_start`, which orders the write before their next
        // read. The lz thread itself never holds this borrow across `create`,
        // which takes `&mut MatchFinderMt`.
        unsafe { &*self.common.get() }
    }

    /// An empty shared state: no buffers, and both threads' sync objects
    /// fresh. [`MatchFinderMt::create`] fills it in.
    fn empty() -> Self {
        MtShared {
            common: UnsafeCell::new(Common::empty()),
            block: UnsafeCell::new(BlockInput {
                ptr: core::ptr::null(),
                len: 0,
                pos: 0,
            }),
            hash_state: UnsafeCell::new(HashState {
                pos: 0,
                buffer: 0,
                stream_pos: 0,
                stream_end_was_reached: false,
                result: Ok(()),
                failed: false,
            }),
            bt_state: UnsafeCell::new(BtState {
                hash_buf_pos: 0,
                hash_buf_pos_limit: 0,
                hash_num_avail: 0,
                failure: false,
                pos: 0,
                buffer: 0,
                cyclic_buffer_pos: 0,
            }),
            lz_state: UnsafeCell::new(LzState {
                pointer_to_cur_pos: 0,
                bt_buf_pos: 0,
                bt_buf_pos_limit: 0,
                lz_pos: 0,
                bt_num_avail_bytes: 0,
                failure_lz_bt: false,
            }),
            hash_sync: MtSync::new(),
            bt_sync: MtSync::new(),
            bt_shift: AtomicUsize::new(0),
            lz_shift: AtomicUsize::new(0),
            thread_failed: AtomicBool::new(false),
            read_result: Mutex::new(Ok(())),
            own: UnsafeCell::new(Owned::default()),
        }
    }
}

/// What the hash thread of a pair kept across blocks reads: the block
/// [`run_block`] set, a slice at a time.
struct BlockSource<'a> {
    sh: &'a MtShared,
}

impl SeqInStream for BlockSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        // SAFETY: this runs on the hash thread during a stream, when `block`
        // is that thread's: `run_block` wrote it before the stream started and
        // does not touch it again until both producer threads have stopped.
        let b = unsafe { &mut *self.sh.block.get() };
        let n = buf.len().min(b.len - b.pos);
        if n != 0 {
            // SAFETY: `ptr..ptr + len` is the block `run_block` borrows for
            // the whole stream, and `pos + n <= len`.
            let src = unsafe { core::slice::from_raw_parts(b.ptr.add(b.pos), n) };
            buf[..n].copy_from_slice(src);
            b.pos += n;
        }
        Ok(n)
    }
}

impl MtShared {
    /// C: `MatchFinder_NeedMove`.
    fn need_move(&self, h: &HashState) -> bool {
        if self.common().direct || h.stream_end_was_reached || h.result.is_err() {
            return false;
        }
        (self.common().win_len - h.buffer) <= self.common().keep_size_after as usize
    }

    /// C: `MatchFinder_MoveBlock`, with the two extra index fixups
    /// `HashThreadFunc` does around it.
    ///
    /// The one operation that is not partitioned: it slides the whole window
    /// down, so the bt and lz threads' indices into it move too. The caller
    /// holds both critical sections, which is what stops them.
    fn move_block(&self, h: &mut HashState) {
        let c = self.common();
        if h.buffer < c.keep_size_before as usize {
            // As `MatchFinder::move_block`: a window cut to a promised stream
            // length, and a stream that broke the promise. Ending the stream
            // with an error keeps the copy below inside the window.
            self.fail_read(h, Error::InternalFailure);
            return;
        }
        let offset = h.buffer - c.keep_size_before as usize;
        let keep_before = (offset & (K_BLOCK_MOVE_ALIGN - 1)) + c.keep_size_before as usize;
        let from = offset & !(K_BLOCK_MOVE_ALIGN - 1);
        let len = keep_before + h.avail() as usize;
        // SAFETY: both critical sections are held, so neither the bt nor the
        // lz thread is inside `Common::win`; `from + len` is within the window
        // because `MatchFinder_Create` sized it that way. The copy overlaps,
        // which is what `copy` (C: `memmove`) is for.
        unsafe {
            core::ptr::copy(c.win.add(from), c.win, len);
        }
        let shift = h.buffer - keep_before;
        h.buffer = keep_before;
        // C: `mt->pointerToCurPos -= offset; mt->buffer -= offset;`, where the
        // C's `offset` is the distance the window actually moved. The owners
        // apply it themselves when they next take their section
        // (`apply_bt_shift`, `apply_lz_shift`): writing their cells from here
        // races the `&mut` each of them holds across `get_next_block`.
        self.bt_shift.fetch_add(shift, Ordering::SeqCst);
        self.lz_shift.fetch_add(shift, Ordering::SeqCst);
    }

    /// The bt thread's half of `move_block`: called each time it has just
    /// entered `hash_sync.cs`, before it looks at the window.
    #[inline]
    fn apply_bt_shift(&self, b: &mut BtState) {
        b.buffer -= self.bt_shift.swap(0, Ordering::SeqCst);
    }

    /// The lz thread's half of `move_block`: called each time it has just
    /// entered `bt_sync.cs`.
    #[inline]
    fn apply_lz_shift(&self, l: &mut LzState) {
        l.pointer_to_cur_pos -= self.lz_shift.swap(0, Ordering::SeqCst);
    }

    /// C: `MatchFinder_ReadBlock`, against the window through its one raw
    /// pointer rather than through a `&mut Vec`, so that the bt and lz
    /// threads' reads below `streamPos` keep their provenance.
    fn read_block(&self, h: &mut HashState, stream: &mut dyn SeqInStream) {
        if h.stream_end_was_reached || h.result.is_err() {
            return;
        }
        let c = self.common();
        if c.direct {
            // C: the `directInput` branch: the bytes are already in the
            // window, so reading them is only moving `streamPos` over them.
            // SAFETY: this runs on the hash thread during a stream, when
            // `block` is that thread's; see `BlockSource::read`.
            let b = unsafe { &mut *self.block.get() };
            let n = ((u32::MAX - h.avail()) as usize).min(b.len - b.pos);
            h.stream_pos = h.stream_pos.wrapping_add(n as u32);
            b.pos += n;
            if b.pos == b.len {
                h.stream_end_was_reached = true;
            }
            return;
        }
        loop {
            let dest = h.buffer + h.avail() as usize;
            let size = c.win_len - dest;
            if size == 0 {
                // C: "we call ReadBlock() after NeedMove() and MoveBlock(). So
                // we don't execute this branch in normal code flow."
                return;
            }
            // SAFETY: `dest` is at or above `streamPos`, which is where the bt
            // and lz threads stop reading, so this range is disjoint from
            // anything they hold. It is inside the window because
            // `dest + size == win_len`.
            let out = unsafe { core::slice::from_raw_parts_mut(c.win.add(dest), size) };
            match stream.read(out) {
                Err(e) => {
                    self.fail_read(h, e);
                    return;
                }
                Ok(0) => {
                    h.stream_end_was_reached = true;
                    return;
                }
                Ok(n) => {
                    h.stream_pos = h.stream_pos.wrapping_add(n as u32);
                    if h.avail() > c.keep_size_after {
                        return;
                    }
                }
            }
        }
    }

    /// C: `MatchFinder_ReadIfRequired`.
    fn read_if_required(&self, h: &mut HashState, stream: &mut dyn SeqInStream) {
        if self.common().keep_size_after >= h.avail() {
            self.read_block(h, stream);
        }
    }

    /// C: `mf->result = res`: record a read error, for this thread in
    /// `h.result` and for the lz thread in `read_result`.
    fn fail_read(&self, h: &mut HashState, e: Error) {
        h.result = Err(e);
        *self.read_result.lock().unwrap_or_else(|p| p.into_inner()) = Err(e);
    }

    /// The hash thread caught a panic: end the stream here and tell the lz
    /// thread, whose `CheckErrors` turns it into an error.
    fn fail_hash(&self, h: &mut HashState) {
        h.failed = true;
        h.stream_end_was_reached = true;
        self.thread_failed.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// HASH THREAD
// ---------------------------------------------------------------------------

/// C: `HashThreadFunc`, reached through `HashThreadFunc2`.
fn hash_thread_func(sh: &MtShared, stream: &mut dyn SeqInStream) {
    let p = &sh.hash_sync;
    loop {
        let mut block_index: u32 = 0;
        p.can_start.wait();
        if p.exiting() {
            return;
        }

        // SAFETY: this is the hash thread, and `can_start` has just ordered it
        // behind the lz thread's `MatchFinderMt_Init`, so nothing else holds
        // either of these.
        let h = unsafe { &mut *sh.hash_state.get() };
        // C: `MatchFinder_Init_HighHash`.
        // SAFETY: the high hash is the hash thread's exclusive range.
        let high = unsafe { sh.common().high_hash() };
        high[..=sh.common().hash_mask as usize].fill(K_EMPTY_HASH_VALUE);

        loop {
            if sh.need_move(h) {
                // C: both sections, in this order, so that neither the bt nor
                // the lz thread is looking at the window while it slides.
                sh.bt_sync.cs.enter();
                sh.hash_sync.cs.enter();
                sh.move_block(h);
                sh.hash_sync.cs.leave();
                sh.bt_sync.cs.leave();
                continue;
            }

            p.free.wait();

            if p.exiting() {
                // C: "exit is unexpected here. But we check it here for some
                // failure case".
                return;
            }
            // C: "for faster stop : we check (p->stopWriting) after
            // Wait(freeSemaphore)".
            if p.stopping() {
                break;
            }

            if !h.failed {
                // C: `MatchFinder_ReadIfRequired`. The stream is the caller's
                // code; a panic in it must end this stream with an error, not
                // leave the bt thread waiting for a block that never comes.
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sh.read_if_required(h, stream);
                }));
                if read.is_err() {
                    sh.fail_hash(h);
                }
            }
            let offset = hash_block_offset(block_index);
            block_index = block_index.wrapping_add(1);
            if !h.failed {
                let filled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hash_fill_block(sh, h, offset);
                }));
                if filled.is_err() {
                    sh.fail_hash(h);
                }
            }
            if h.failed {
                // Not an end of stream: earlier blocks already promised the
                // bt and lz threads every byte read so far, and an orderly
                // `{2, 0}` here would take back the ones not hashed yet,
                // leaving the lz thread to walk past the last entry the bt
                // thread published. A count below the two-word header is one
                // no real block carries; the bt thread turns it into
                // `failure_BT`, whose empty blocks put the lz thread on the
                // C's `failureBuf`, which is safe to read for any number of
                // positions.
                // SAFETY: the `free` semaphore was taken above, so this block
                // is this thread's until `filled` is released below.
                let heads = unsafe { sh.common().hash_block(offset) };
                heads[0] = 0;
                heads[1] = 0;
            }

            p.filled.release1();
        }

        p.was_stopped.set();
    }
}

/// One pass of `HashThreadFunc`'s loop on the caller's thread, for the
/// inline finder: slide the window if it needs it, read, and fill the one
/// `hash_buf` block. The C's order, without the semaphores or the critical
/// sections: nothing else runs, and nothing holds the window while this
/// moves it.
///
/// C: the body of the inner `for (;;)` in `HashThreadFunc`
/// (`C/LzFindMt.c:466-542`).
fn hash_stage_inline(sh: &MtShared, stream: &mut dyn SeqInStream) -> usize {
    // SAFETY: inline, the caller's thread is the only one there is, and it
    // enters the hash thread's cell only here, from the bt stage, which holds
    // `bt_state` and nothing else.
    let h = unsafe { &mut *sh.hash_state.get() };
    // C: `if (MatchFinder_NeedMove(mf)) { ... MatchFinder_MoveBlock(mf); ...
    // continue; }`, which comes straight back here.
    while sh.need_move(h) {
        sh.move_block(h);
    }
    sh.read_if_required(h, stream);
    hash_fill_block(sh, h, 0);
    0
}

/// One `hash_buf` block of `HashThreadFunc`'s loop: the heads for the next
/// run of positions, or the end-of-stream header once the bytes run out.
fn hash_fill_block(sh: &MtShared, h: &mut HashState, offset: usize) {
    let c = sh.common();
    let mut num = h.avail();

    // C: "heads[1] contains the number of avail bytes: if (avail <
    // mf->numHashBytes) it means that stream was finished [...]
    // HASH_THREAD fills only the header (2 numbers) for all next
    // blocks: {2, NumHashBytes - 1}, {2,0}, {2,0}, ..."
    // SAFETY: the caller has just taken the `free` semaphore for this
    // block, so it is this thread's until the caller releases `filled`.
    let heads = unsafe { c.hash_block(offset) };
    heads[0] = 2;
    heads[1] = num;

    if num >= c.num_hash_bytes {
        num = num - c.num_hash_bytes + 1;
        if num > HASH_BLOCK_SIZE - 2 {
            num = HASH_BLOCK_SIZE - 2;
        }

        if h.pos > MT_MAX_VAL_FOR_NORMALIZE - num {
            let sub_value = h.pos - c.history_size - 1;
            // C: `MatchFinder_REDUCE_OFFSETS`.
            h.pos -= sub_value;
            h.stream_pos -= sub_value;
            // SAFETY: the high hash is this thread's range.
            let hash = unsafe { c.high_hash() };
            crate::enc::lz_find::normalize3(sub_value, &mut hash[..=c.hash_mask as usize]);
        }

        heads[0] = 2 + num;
        // SAFETY: the window below `stream_pos` is stable here:
        // this thread is the only writer and is not writing, and
        // `move_block` cannot run while this block is held.
        let win = unsafe { c.win() };
        // SAFETY: the high hash is this thread's range.
        let hash = unsafe { c.high_hash() };
        get_heads(
            c.heads,
            &win[h.buffer..],
            h.pos,
            hash,
            c.hash_mask,
            &mut heads[2..2 + num as usize],
            &c.crc,
        );
    }

    // C: "wrap over zero is allowed at the end of stream".
    h.pos = h.pos.wrapping_add(num);
    h.buffer += num as usize;
}

// ---------------------------------------------------------------------------
// BT THREAD
// ---------------------------------------------------------------------------

/// C: `BtGetMatches`. Fills one `bt_buf` block from the head distances the
/// hash thread produced, pulling more `hash_buf` blocks as it needs them.
///
/// `next_heads` hands over the next filled `hash_buf` block and returns its
/// offset: on the bt thread that is `MtSync_GetNextBlock(&p->hashSync)`, and
/// inline it runs the hash stage for one block ([`hash_stage_inline`]).
fn bt_get_matches(
    sh: &MtShared,
    b: &mut BtState,
    block_offset: usize,
    mut next_heads: impl FnMut() -> usize,
) {
    let c = sh.common();
    let mut num_processed: u32 = 0;
    let mut cur_pos: u32 = 2;

    // C: "GetMatchesSpec() functions don't create (len = 1) in [len, dist]
    // match pairs, if (p->numHashBytes >= 2)".
    let limit = BT_BLOCK_SIZE - c.match_max_len * 2;

    // SAFETY: this block of `bt_buf` is the bt thread's until it releases
    // `filled`, and the `hash_buf` blocks it reads are held the same way.
    let d = unsafe { c.bt_block(block_offset) };
    d[1] = b.hash_num_avail;

    if b.failure {
        d[0] = 0;
        return;
    }

    #[cfg(test)]
    tests::maybe_panic_in_bt(c.cyclic_buffer_size);

    while cur_pos < limit {
        if b.hash_buf_pos == b.hash_buf_pos_limit {
            let avail;
            {
                let k = next_heads() as u32;
                // The window may have slid while `hash_sync.cs` was open, or
                // inline, while `next_heads` ran the hash stage.
                sh.apply_bt_shift(b);
                // SAFETY: `get_next_block` has just handed this block over.
                let h = unsafe { c.hash_block(k as usize) };
                if h[0] < 2 {
                    // The hash thread failed (see `hash_thread_func`). Not in
                    // the C, whose hash thread cannot fail: handled as its
                    // "internal data failure" below.
                    b.failure = true;
                    d[0] = 0;
                    return;
                }
                avail = h[1];
                b.hash_buf_pos_limit = k + h[0];
                b.hash_num_avail = avail;
                b.hash_buf_pos = k + 2;
            }

            // C: "we must prevent UInt32 overflow for avail total value".
            let sum = num_processed.wrapping_add(avail);
            d[1] = if sum < num_processed { u32::MAX } else { sum };

            if avail >= c.num_hash_bytes {
                continue;
            }

            // C: "(avail < p->numHashBytes) It means that stream was finished.
            // [...] we fill (d) for (avail) bytes for LZ_THREAD (receiver)."
            b.hash_num_avail = 0;
            d[0] = cur_pos + avail;
            let base = cur_pos as usize;
            d[base..base + avail as usize].fill(0);
            return;
        }

        let mut size = b.hash_buf_pos_limit - b.hash_buf_pos;
        let mut pos = b.pos;
        let mut cyclic_buffer_pos = b.cyclic_buffer_pos;
        let mut len_limit = c.match_max_len;
        if len_limit >= b.hash_num_avail {
            len_limit = b.hash_num_avail;
        }
        {
            let size2 = b.hash_num_avail - len_limit + 1;
            if size2 < size {
                size = size2;
            }
            let size2 = c.cyclic_buffer_size - cyclic_buffer_pos;
            if size2 < size {
                size = size2;
            }
        }

        if pos > MT_MAX_VAL_FOR_NORMALIZE - size {
            let sub_value = pos - c.cyclic_buffer_size;
            pos -= sub_value;
            b.pos = pos;
            // SAFETY: `son` is the bt thread's exclusive range.
            let son = unsafe { c.son() };
            crate::enc::lz_find::normalize3(sub_value, son);
        }

        let pos_res;
        let d_end;
        {
            // SAFETY: the window below `stream_pos` is stable while this
            // thread holds a `hash_buf` block: `move_block` waits on
            // `hash_sync.cs`, which `get_next_block` left locked.
            let win = unsafe { c.win() };
            // SAFETY: `son` is the bt thread's exclusive range.
            let son = unsafe { c.son() };
            // The `hash_buf` block `hash_buf_pos` is in: `hash_buf_pos` stays
            // below `hash_buf_pos_limit`, which is at most the block's end.
            let head_base = b.hash_buf_pos as usize & !(HASH_BLOCK_SIZE as usize - 1);
            // SAFETY: the `hash_buf` block this reads is held, as above.
            let heads = unsafe { c.hash_block(head_base) };
            let head_pos = b.hash_buf_pos as usize - head_base;
            // The kernel tests five conditions before its unchecked accesses
            // (its "# Bounds"). Why each holds here, at every call:
            //
            // A, the run ends inside the cyclic buffer: `size` was cut to
            // `cyclic_buffer_size - cyclic_buffer_pos` above.
            // B, the tree holds two sons a slot: the table plan gives a
            // binary-tree finder `2 * cyclic_buffer_size` of them, and the
            // threaded finder is only ever a binary tree.
            // C, the hashed bytes fit below the limit: heads exist only for
            // positions with `num_hash_bytes` bytes after them, so
            // `hash_num_avail >= num_hash_bytes` while any remain, and
            // `match_max_len` is no smaller; `len_limit` is the lesser.
            // D, the run ends inside the window: `size` was cut to
            // `hash_num_avail - len_limit + 1` above, which makes the last
            // index the kernel can reach `b.buffer + hash_num_avail - 1`,
            // below the stream position the hash thread had published.
            // E, no distance reaches before the window: `b.buffer` starts at
            // zero with `pos` at one and they advance together, so
            // `pos <= b.buffer + 1` until a window slide. A slide leaves the
            // hash thread at `keep_size_before` or later and this thread less
            // than `HASH_BUFFER_SIZE` behind it, and `keep_size_before` is
            // the cyclic size plus that and more; so afterwards
            // `cyclic_buffer_size <= b.buffer`. Normalisation only lowers
            // `pos`, to exactly `cyclic_buffer_size`.
            match get_matches_spec_n_2(
                win,
                b.buffer + len_limit as usize - 1,
                pos,
                b.buffer,
                son,
                c.cut_value,
                d,
                cur_pos as usize,
                c.num_hash_bytes as usize - 1,
                heads,
                head_pos,
                limit as usize,
                head_pos + size as usize,
                cyclic_buffer_pos,
                c.cyclic_buffer_size,
            ) {
                Some((di, pr)) => {
                    d_end = di;
                    pos_res = pr;
                }
                None => {
                    // C: "internal data failure".
                    b.failure = true;
                    d[0] = 0;
                    return;
                }
            }
        }

        cur_pos = d_end as u32;
        {
            let processed = pos_res - pos;
            pos = pos_res;
            b.hash_buf_pos += processed;
            cyclic_buffer_pos += processed;
            b.buffer += processed as usize;
        }

        {
            let processed = pos - b.pos;
            num_processed += processed;
            b.hash_num_avail -= processed;
            b.pos = pos;
        }
        if cyclic_buffer_pos == c.cyclic_buffer_size {
            cyclic_buffer_pos = 0;
        }
        b.cyclic_buffer_pos = cyclic_buffer_pos;
    }

    d[0] = cur_pos;
}

/// C: `BtFillBlock`.
///
/// A panic inside `BtGetMatches` is caught here and becomes the C's
/// `failure_BT`: this block and every later one is handed over empty, which
/// the lz thread takes as `failure_LZ_BT`. The buffer is still locked at that
/// point - nothing between `get_next_block`'s unlock and its relock can panic
/// - so the unlock below stays balanced.
fn bt_fill_block(sh: &MtShared, b: &mut BtState, global_block_index: u32) {
    let sync = &sh.hash_sync;
    if !MtSync::get(&sync.need_start) {
        sync.lock_buffer();
        // The window may have slid while `hash_sync.cs` was open.
        sh.apply_bt_shift(b);
    }
    let block_offset = bt_block_offset(global_block_index);
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bt_get_matches(sh, b, block_offset, || {
            hash_block_offset(sh.hash_sync.get_next_block())
        });
    }));
    if run.is_err() {
        b.failure = true;
        sh.thread_failed.store(true, Ordering::SeqCst);
        // SAFETY: this block of `bt_buf` is still the bt thread's; `filled`
        // is released only after this returns.
        let d = unsafe { sh.common().bt_block(block_offset) };
        d[0] = 0;
    }
    // C: "We suppose that we have called GetNextBlock() from start. So buffer
    // is LOCKED".
    sync.unlock_buffer();
}

/// C: `BtThreadFunc`, reached through `BtThreadFunc2`.
fn bt_thread_func(sh: &MtShared) {
    let p = &sh.bt_sync;
    loop {
        let mut block_index: u32 = 0;
        p.can_start.wait();

        // SAFETY: this is the bt thread; `can_start` orders it behind the lz
        // thread's `MatchFinderMt_Init`, and nothing else enters this cell.
        let b = unsafe { &mut *sh.bt_state.get() };

        loop {
            // C: "(p->exit == true) is possible after (p->canStart) at first
            // loop iteration".
            if p.exiting() {
                return;
            }
            p.free.wait();
            if p.stopping() {
                break;
            }
            bt_fill_block(sh, b, block_index);
            block_index = block_index.wrapping_add(1);
            p.filled.release1();
        }

        // C: "we stop HASH_THREAD here".
        sh.hash_sync.stop_writing();
        p.was_stopped.set();
    }
}

// ---------------------------------------------------------------------------
// LZ THREAD - the public face
// ---------------------------------------------------------------------------

/// The buffer layout and the keep sizes for one mode: `(hash_buf words,
/// bt_buf words, extra keep before, extra keep after)`.
///
/// Threaded, the C's (`MatchFinderMt_Create`, `C/LzFindMt.c:856-879`): two
/// hash blocks and sixteen bt blocks, and a window kept back by both rings,
/// because the lz thread may be that far behind the hash thread when it
/// slides the window.
///
/// Inline, one block of each, and the window kept back by one bt block. The
/// hash stage runs only when the bt stage has used every head it was given,
/// so it slides the window with the bt stage at its own position, and the lz
/// stage at most the bt block it is filling behind: fewer than
/// `BT_BLOCK_SIZE` positions, as every position takes at least one word of
/// it. The keep after is the C's in both modes: the hash stage hashes up to
/// a block ahead of what it read, and every position it hashes still needs
/// `keepAddBufferAfter` bytes behind it, for the same matches as the
/// single-threaded finder's.
const fn layout(inline: bool) -> (usize, usize, u32, u32) {
    if inline {
        (
            HASH_BLOCK_SIZE as usize,
            BT_BLOCK_SIZE as usize,
            BT_BLOCK_SIZE,
            HASH_BLOCK_SIZE,
        )
    } else {
        (
            HASH_BUFFER_SIZE,
            BT_BUFFER_SIZE,
            (HASH_BUFFER_SIZE + BT_BUFFER_SIZE) as u32,
            HASH_BLOCK_SIZE,
        )
    }
}

/// The threaded match finder.
///
/// C: `CMatchFinderMt` plus the `IMatchFinder2` vtable
/// `MatchFinderMt_CreateVTable` fills in. Everything here runs on the lz
/// thread - the one that called the encoder.
pub(crate) struct MatchFinderMt {
    /// C: `MFB`, the `CMatchFinder` `LzmaEnc` configures and
    /// `MatchFinderMt_Create` hands to `MatchFinder_Create`. It keeps the
    /// settings between calls; its allocations move into `sh` on `create`.
    pub(crate) mfb: MatchFinder,
    sh: Option<Arc<MtShared>>,
    /// C: `hashSync.thread` and `btSync.thread`. The pair [`Self::block_handle`]
    /// starts and keeps for every block after, as the C keeps its threads
    /// from `MatchFinderMt_Create` to `MatchFinderMt_Destruct`; `None` while
    /// none runs. The stream path starts a pair of its own per stream in
    /// [`with_threads`], and retires this one first.
    threads: Option<(std::thread::JoinHandle<()>, std::thread::JoinHandle<()>)>,
    /// How many pairs this finder has started for blocks.
    #[cfg(test)]
    pub(crate) spawns: u32,
}

impl Drop for MatchFinderMt {
    fn drop(&mut self) {
        self.retire_threads();
    }
}

impl MatchFinderMt {
    /// The `CMatchFinder` this finder was configured through, holding the
    /// window and table this finder last allocated, for
    /// [`crate::enc::finder::Finder::make_st`], and the hand-off buffers
    /// parked in it. The threads are stopped.
    ///
    /// C: there is one `MFB` for both finders (`C/LzmaEnc.c:494`), so the
    /// single-threaded `MatchFinder_Create` (`C/LzmaEnc.c:2761`) finds the
    /// window and table the threaded one left and keeps them while they are
    /// long enough (`C/LzFind.c:394`, `C/LzFind.c:479`).
    pub(crate) fn into_st(mut self) -> MatchFinder {
        self.retire_threads();
        let mut mfb = core::mem::replace(&mut self.mfb, MatchFinder::new());
        if let Some(sh) = self.sh.take() {
            // SAFETY: as for `own` in `create`: the threads of a block pair
            // were joined just above, a stream's are joined when its stream
            // ends, and nothing else holds `sh`.
            let own = unsafe {
                *sh.common.get() = Common::empty();
                &mut *sh.own.get()
            };
            if mfb.buf_base.is_empty() {
                mfb.buf_base = core::mem::take(&mut own.win);
            }
            if mfb.hash.is_empty() {
                mfb.hash = core::mem::take(&mut own.tab);
            }
            mfb.mt_bufs = core::mem::take(&mut own.bufs);
        }
        mfb
    }

    /// C: `MatchFinderMt_Construct`.
    pub(crate) fn new() -> Self {
        MatchFinderMt {
            mfb: MatchFinder::new(),
            sh: None,
            threads: None,
            #[cfg(test)]
            spawns: 0,
        }
    }

    fn shared(&self) -> &MtShared {
        self.sh.as_deref().expect("match finder was not created")
    }

    /// C: `MatchFinderMt_Create`. `direct` is the C's `directInput`: the
    /// stream is a block [`run_block`] will hand the finder in place, and no
    /// window is allocated for it. `inline` asks for no threads: the caller's
    /// thread runs both producer stages a block at a time, and one block of
    /// each buffer is allocated (see [`layout`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create(
        &mut self,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
        data_limit: u64,
        direct: bool,
        inline: bool,
    ) -> Result<(), Error> {
        if BT_BLOCK_SIZE <= match_max_len * 4 {
            return Err(Error::Param);
        }
        if inline {
            // A pair kept for blocks by an earlier threaded `create` must
            // not wait on this finder's buffers: an inline one never stops
            // it.
            self.retire_threads();
        }
        let (hash_words, bt_words, add_before, add_after) = layout(inline);
        let bufs_len = hash_words + bt_words + 2;

        // C: `MatchFinderMt_Create` keeps `hashBuf` once it has it, and
        // `MatchFinder_Create` keeps a window and tables that are long enough.
        // The block before left all three in `sh`; they go back to where the
        // C keeps them, so that a second block allocates nothing, and come
        // back to `sh` below. `sh` itself is kept: a pair of threads kept
        // across blocks waits on its sync objects.
        let sh = self
            .sh
            .take()
            .unwrap_or_else(|| Arc::new(MtShared::empty()));
        // SAFETY: `create` runs on the lz thread between streams. The
        // producer threads of a stream path have been joined, and a pair kept
        // across blocks is waiting on `can_start`, which only the lz thread's
        // next `get_next_block` sets; neither reads `common` or `own` until
        // then.
        let own = unsafe {
            // The buffers leave `sh` here, so nothing may point into them
            // from it until they come back.
            *sh.common.get() = Common::empty();
            &mut *sh.own.get()
        };
        // A finder [`crate::enc::finder::Finder::make_mt`] has just made
        // holds nothing in `sh` yet, and its `mfb` holds what the
        // single-threaded finder allocated: those are kept, as the C's one
        // `MFB` keeps them whichever finder `LzmaEnc_Alloc` picks
        // (`C/LzmaEnc.c:494`, `C/LzFindMt.c:872`).
        if !own.win.is_empty() {
            self.mfb.buf_base = core::mem::take(&mut own.win);
        }
        if !own.tab.is_empty() {
            self.mfb.hash = core::mem::take(&mut own.tab);
        }
        let mut bufs = core::mem::take(&mut own.bufs);
        if bufs.is_empty() {
            bufs = core::mem::take(&mut self.mfb.mt_bufs);
        }
        self.sh = Some(Arc::clone(&sh));
        if bufs.len() == bufs_len {
            // Every word of a hash block and of a bt block is written by the
            // thread that fills it before the thread it is handed to reads it.
            // The two words past `btBuf` are the exception: the lz thread
            // parks on them after a failure.
            bufs[hash_words + bt_words..].fill(0);
        } else {
            // Zeroed by the allocator, not by a fill: a block is written
            // before it is read, so a stream shorter than a block leaves the
            // rest of it untouched and never resident. The inline finder's
            // one block of each is about 0.75 MiB that a small stream would
            // otherwise pay in full.
            // The old buffers go first, so the two are never held at once.
            drop(core::mem::take(&mut bufs));
            bufs = crate::enc::lz_find::zeroed_vec(bufs_len)?;
            #[cfg(test)]
            {
                self.mfb.allocs += 1;
            }
        }

        let before = keep_add_buffer_before
            .checked_add(add_before)
            .ok_or(Error::Param)?;
        let after = keep_add_buffer_after
            .checked_add(add_after)
            .ok_or(Error::Param)?;
        self.mfb.direct_input = direct;
        self.mfb
            .create(history_size, before, match_max_len, after, data_limit)?;

        // C: `MFB.bigHash = (MFB.hashMask >= 0xFFFFFF)`, set after
        // `MatchFinderMt_Create` and read by `MatchFinderMt_CreateVTable`.
        // `C/LzFind.c` sizes the table so that the `b` variants' unmasked
        // 24-bit term always fits once this is true.
        self.mfb.big_hash = self.mfb.hash_mask >= 0xFF_FFFF;

        let heads = match (self.mfb.num_hash_bytes, self.mfb.big_hash) {
            (2, _) => Heads::H2,
            (3, false) => Heads::H3,
            (3, true) => Heads::H3b,
            (4, false) => Heads::H4,
            (4, true) => Heads::H4b,
            (_, false) => Heads::H5,
            (_, true) => Heads::H5b,
        };
        let mix = match self.mfb.num_hash_bytes {
            2 => Mix::None,
            3 => Mix::Two,
            4 => Mix::Three,
            _ => Mix::Four,
        };

        let mut win = core::mem::take(&mut self.mfb.buf_base);
        let mut tab = core::mem::take(&mut self.mfb.hash);
        let son_base = self.mfb.son_base;
        let common = Common {
            // With `direct` the window is the block, which `run_block` sets.
            win: if direct {
                core::ptr::NonNull::dangling().as_ptr()
            } else {
                win.as_mut_ptr()
            },
            // `MatchFinder::create` keeps an allocation that is longer than
            // this window needs; the window is `block_size` of it.
            win_len: if direct {
                0
            } else {
                self.mfb.block_size as usize
            },
            direct,
            tab: tab.as_mut_ptr(),
            bufs: bufs.as_mut_ptr(),
            bt_base: hash_words,
            park_base: hash_words + bt_words,
            inline,
            hash_mask: self.mfb.hash_mask,
            fixed_hash_size: self.mfb.fixed_hash_size as usize,
            son_base,
            son_len: tab.len() - son_base,
            history_size,
            num_hash_bytes: self.mfb.num_hash_bytes,
            match_max_len: self.mfb.match_max_len,
            cut_value: self.mfb.cut_value,
            cyclic_buffer_size: self.mfb.cyclic_buffer_size,
            keep_size_before: self.mfb.keep_size_before,
            keep_size_after: self.mfb.keep_size_after,
            crc: self.mfb.crc,
            heads,
            mix,
        };

        // SAFETY: as for `own` above.
        unsafe {
            *sh.common.get() = common;
            *sh.own.get() = Owned { win, tab, bufs };
        }
        Ok(())
    }

    /// What [`MatchFinderMt::create`] would allocate for this configuration,
    /// in bytes: `hashBuf` and `btBuf`, and the window and tables of a
    /// `MatchFinder_Create` given the two enlarged keep sizes, the window
    /// left out with `direct`, as [`MatchFinderMt::create`] leaves it out.
    /// `mfb` is the `CMatchFinder` carrying the settings; nothing is
    /// allocated.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mem_usage(
        mfb: &mut MatchFinder,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
        direct: bool,
        inline: bool,
    ) -> Result<u64, Error> {
        if BT_BLOCK_SIZE <= match_max_len * 4 {
            return Err(Error::Param);
        }
        let (hash_words, bt_words, add_before, add_after) = layout(inline);
        let before = keep_add_buffer_before
            .checked_add(add_before)
            .ok_or(Error::Param)?;
        let after = keep_add_buffer_after
            .checked_add(add_after)
            .ok_or(Error::Param)?;
        let base = mfb.mem_usage(history_size, before, match_max_len, after, !direct)?;
        Ok(base + (hash_words + bt_words + 2) as u64 * 4)
    }

    /// What this finder has allocated, in bytes: the window, the tables, and
    /// `hashBuf` with `btBuf`.
    #[cfg(test)]
    pub(crate) fn allocated(&self) -> u64 {
        match &self.sh {
            Some(sh) => {
                // SAFETY: the lz thread, outside a stream; see `create`.
                let own = unsafe { &*sh.own.get() };
                own.win.len() as u64 + (own.tab.len() as u64 + own.bufs.len() as u64) * 4
            }
            None => self.mfb.allocated(),
        }
    }

    /// The window this finder has allocated, and the length of the one its
    /// threads read now, in bytes.
    #[cfg(test)]
    pub(crate) fn windows(&self) -> (usize, usize) {
        let sh = self.shared();
        // SAFETY: the lz thread, outside a stream; see `create`.
        let own = unsafe { &*sh.own.get() };
        (own.win.len(), sh.common().win_len)
    }

    /// C: `MatchFinderMt_InitMt`, "call it before `IMatchFinder::Init()`".
    pub(crate) fn init_mt(&self) -> Result<(), Error> {
        let sh = self.shared();
        if sh.common().inline {
            // No threads, so no semaphores to count blocks with.
            return Ok(());
        }
        sh.hash_sync.init(HASH_NUM_BLOCKS)?;
        sh.bt_sync.init(BT_NUM_BLOCKS)
    }

    /// C: `MatchFinderMt_Init`. Deliberately reads nothing: "Init without data
    /// reading. We don't want to read data in this thread."
    pub(crate) fn init(&mut self) {
        let sh = self.shared();
        let c = sh.common();
        // SAFETY: the lz thread owns `lz_state`; the other two cells are only
        // reachable here because both producer threads are stopped - `init` is
        // called between `init_mt` and the first `get_next_block`, and neither
        // thread has been let past `can_start` yet.
        unsafe {
            let h = &mut *sh.hash_state.get();
            // C: `MatchFinder_Init_4`.
            h.buffer = 0;
            h.pos = 1;
            h.stream_pos = 1;
            h.result = Ok(());
            h.stream_end_was_reached = false;
            h.failed = false;

            // C: `MatchFinder_Init_LowHash`.
            c.low_hash().fill(K_EMPTY_HASH_VALUE);
            if c.inline {
                // C: `MatchFinder_Init_HighHash`, which the hash thread runs
                // when it starts (`C/LzFindMt.c:464`); inline the hash stage
                // is this thread's.
                c.high_hash()[..=c.hash_mask as usize].fill(K_EMPTY_HASH_VALUE);
            }

            let b = &mut *sh.bt_state.get();
            b.hash_buf_pos = 0;
            b.hash_buf_pos_limit = 0;
            b.hash_num_avail = 0;
            b.failure = false;
            // C: "we must init (p->pos = mf->pos) for BT, because BT code
            // needs (p->pos == delta_value_for_empty_hash_record == mf->pos)".
            b.pos = h.pos;
            // C: `CYC_TO_POS_OFFSET` is 0.
            b.cyclic_buffer_pos = b.pos;
            b.buffer = h.buffer;

            let l = &mut *sh.lz_state.get();
            l.bt_buf_pos = 0;
            l.bt_buf_pos_limit = 0;
            l.pointer_to_cur_pos = h.buffer;
            l.bt_num_avail_bytes = 0;
            l.failure_lz_bt = false;
            // C: "1; // optimal smallest value".
            l.lz_pos = 1;
        }
        sh.bt_shift.store(0, Ordering::SeqCst);
        sh.lz_shift.store(0, Ordering::SeqCst);
        sh.thread_failed.store(false, Ordering::SeqCst);
        *sh.read_result.lock().unwrap_or_else(|p| p.into_inner()) = Ok(());
    }

    /// The handle [`with_threads`] needs to start a pair of producer threads
    /// for one stream. A pair kept for blocks is retired first: two pairs
    /// must never wait on the same sync objects.
    pub(crate) fn stream_handle(&mut self) -> Option<Arc<MtShared>> {
        self.retire_threads();
        if self.is_inline() {
            // No threads to start: the encoder hands the stream to the
            // finder's own calls instead.
            return None;
        }
        self.sh.clone()
    }

    /// Whether this finder runs its stages on the caller's thread.
    pub(crate) fn is_inline(&self) -> bool {
        self.sh.as_deref().is_some_and(|sh| sh.common().inline)
    }

    /// The handle [`run_block`] needs, with this finder's pair of producer
    /// threads running: started on the first block and kept for every block
    /// after, as the C keeps the threads `MatchFinderMt_Create` started.
    ///
    /// # Errors
    ///
    /// [`Error::Alloc`] if a thread cannot be started.
    pub(crate) fn block_handle(&mut self) -> Result<Option<Arc<MtShared>>, Error> {
        let Some(sh) = self.sh.clone() else {
            return Ok(None);
        };
        // Inline, `run_block` only sets the block the hash stage reads; no
        // thread is started, and its stop finds none running.
        if self.threads.is_none() && !sh.common().inline {
            // Set before the spawns: the bt thread calls
            // `hash_sync.stop_writing()`, which does nothing unless the flag
            // is already up.
            MtSync::set(&sh.hash_sync.was_created, true);
            MtSync::set(&sh.bt_sync.was_created, true);
            let hash_sh = Arc::clone(&sh);
            let hash = std::thread::Builder::new()
                .spawn(move || hash_thread_func(&hash_sh, &mut BlockSource { sh: &hash_sh }))
                .map_err(|_| Error::Alloc)?;
            let bt_sh = Arc::clone(&sh);
            let bt = match std::thread::Builder::new().spawn(move || bt_thread_func(&bt_sh)) {
                Ok(bt) => bt,
                Err(_) => {
                    sh.hash_sync.send_exit();
                    let _ = hash.join();
                    return Err(Error::Alloc);
                }
            };
            self.threads = Some((hash, bt));
            #[cfg(test)]
            {
                self.spawns += 1;
            }
        }
        Ok(Some(sh))
    }

    /// C: `MatchFinderMt_Destruct`'s half that ends the threads: tell the
    /// stopped pair to return and wait for both.
    fn retire_threads(&mut self) {
        if let Some((hash, bt)) = self.threads.take() {
            let sh = self.shared();
            // C: "we want thread to be in Stopped state before sending EXIT
            // command. note: stop(btSync) will stop (htSync) also." Between
            // blocks both already are, and these return at once.
            sh.bt_sync.stop_writing();
            sh.bt_sync.send_exit();
            let _ = bt.join();
            sh.hash_sync.stop_writing();
            sh.hash_sync.send_exit();
            let _ = hash.join();
        }
    }

    /// C: `MatchFinder_GetPointerToCurrentPos`, as an index into
    /// [`MatchFinderMt::window`].
    #[inline]
    pub(crate) fn cur(&self) -> usize {
        // SAFETY: the lz thread owns `lz_state`.
        unsafe { (*self.shared().lz_state.get()).pointer_to_cur_pos }
    }

    /// The window the encoder reads literals and match bytes out of.
    ///
    /// C: the bytes `p->pointerToCurPos` points into.
    #[inline]
    pub(crate) fn window(&self) -> &[u8] {
        // SAFETY: the lz thread may always read the window; see
        // `Common::win`.
        unsafe { self.shared().common().win() }
    }

    /// C: `mf->result`, as `CheckErrors` reads it.
    ///
    /// On a pair kept for blocks the hash thread is still running when the
    /// encoder asks, so this reads the copy the hash thread publishes in
    /// `read_result`, never `hash_state`, which that thread may be holding.
    pub(crate) fn result(&self) -> Result<(), Error> {
        *self
            .shared()
            .read_result
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// C: `p->matchFinderMt.failure_LZ_BT`, which `CheckErrors` turns into
    /// an error, or a panic one of the producer threads caught.
    pub(crate) fn failed(&self) -> bool {
        self.sh.as_deref().is_some_and(|sh| {
            // SAFETY: the lz thread owns `lz_state`.
            let lz_bt = unsafe { (*sh.lz_state.get()).failure_lz_bt };
            lz_bt || sh.thread_failed.load(Ordering::SeqCst)
        })
    }

    /// C: `MatchFinderMt_GetNextBlock_Bt`. Inline, the block is filled here
    /// instead of waited for: the bt stage runs for one block, running the
    /// hash stage over `stream` whenever it has used its heads.
    fn get_next_block_bt(&mut self, stream: &mut dyn SeqInStream) -> u32 {
        let sh = self.shared();
        let c = sh.common();
        // SAFETY: the lz thread owns `lz_state`.
        let l = unsafe { &mut *sh.lz_state.get() };
        if l.failure_lz_bt {
            l.bt_buf_pos = BT_BUFFER_SIZE;
        } else {
            let bi = if c.inline {
                // SAFETY: inline, this thread is the bt thread too, and holds
                // no other borrow of `bt_state`. The block it fills is the
                // one this thread read from last, which it is done with.
                let b = unsafe { &mut *sh.bt_state.get() };
                // C: `BtFillBlock` without the lock, `C/LzFindMt.c:740-757`.
                bt_get_matches(sh, b, 0, || hash_stage_inline(sh, stream));
                0
            } else {
                sh.bt_sync.get_next_block()
            };
            // The window may have slid while `bt_sync.cs` was open, or
            // inline, while the hash stage ran.
            sh.apply_lz_shift(l);
            let base = bt_block_offset(bi);
            // SAFETY: `get_next_block` has just handed this block over, and
            // the lz thread holds it until the next call.
            let bt = unsafe { c.bt_block(base) };
            let num_items = bt[0];
            l.bt_buf_pos_limit = base + num_items as usize;
            l.bt_num_avail_bytes = bt[1];
            l.bt_buf_pos = base + 2;
            if !(2..=BT_BLOCK_SIZE).contains(&num_items) {
                // SAFETY: this is the lz thread.
                let park = unsafe { c.bt_park() };
                park[0] = 0;
                l.bt_buf_pos = BT_BUFFER_SIZE;
                l.bt_buf_pos_limit = BT_BUFFER_SIZE + 1;
                l.failure_lz_bt = true;
                // C: "we don't want to decrease AvailBytes, that was load
                // before".
            }

            if l.lz_pos >= MT_MAX_VAL_FOR_NORMALIZE - BT_BLOCK_SIZE {
                // C: "(fixedHashSize) is small, so normalization is fast".
                let sub_value = l.lz_pos - c.history_size - 1;
                l.lz_pos -= sub_value;
                // SAFETY: the low hash is the lz thread's exclusive range.
                crate::enc::lz_find::normalize3(sub_value, unsafe { c.low_hash() });
            }
        }
        l.bt_num_avail_bytes
    }

    /// C: `GET_NEXT_BLOCK_IF_REQUIRED`.
    #[inline]
    fn next_block_if_required(&mut self, stream: &mut dyn SeqInStream) {
        // SAFETY: the lz thread owns `lz_state`.
        let l = unsafe { &*self.shared().lz_state.get() };
        if l.bt_buf_pos == l.bt_buf_pos_limit {
            self.get_next_block_bt(stream);
        }
    }

    /// C: `MatchFinderMt_GetNumAvailableBytes`. `stream` is read only
    /// inline; threaded, the hash thread holds the input.
    pub(crate) fn get_num_available_bytes(&mut self, stream: &mut dyn SeqInStream) -> u32 {
        // SAFETY: the lz thread owns `lz_state`.
        let l = unsafe { &*self.shared().lz_state.get() };
        if l.bt_buf_pos != l.bt_buf_pos_limit {
            return l.bt_num_avail_bytes;
        }
        self.get_next_block_bt(stream)
    }
}

impl MatchFinderMt {
    /// C: `MixMatches2` / `MixMatches3` / `MixMatches4`.
    ///
    /// The bt thread only tracks matches of at least `numHashBytes` bytes, so
    /// the short ones are found here, from the low hash table, and spliced in
    /// front of the pairs it produced. `match_min_pos` is where the bt
    /// thread's nearest match already is: anything further back is redundant.
    fn mix_matches(&self, match_min_pos: u32, d: &mut [u32], mut di: usize) -> usize {
        let sh = self.shared();
        let c = sh.common();
        // SAFETY: the lz thread owns `lz_state` and the low hash, and may read
        // the window.
        let (l, hash, win) = unsafe { (&*sh.lz_state.get(), c.low_hash(), c.win()) };
        let cur = l.pointer_to_cur_pos;
        let m = l.lz_pos;

        // C: `MT_HASH2_CALC` / `MT_HASH3_CALC`.
        let temp = c.crc[usize::from(win[cur])] ^ u32::from(win[cur + 1]);
        let h2 = (temp & (K_HASH2_SIZE - 1)) as usize;
        let c2 = hash[h2];
        hash[h2] = m;

        if c.mix == Mix::Two {
            if c2 >= match_min_pos && win[cur - (m - c2) as usize] == win[cur] {
                d[di] = 2;
                d[di + 1] = m - c2 - 1;
                di += 2;
            }
            return di;
        }

        let h3 = ((temp ^ (u32::from(win[cur + 2]) << 8)) & (K_HASH3_SIZE - 1)) as usize;
        let c3 = hash[K_FIX3_HASH_SIZE + h3];
        hash[K_FIX3_HASH_SIZE + h3] = m;

        if c.mix == Mix::Three {
            if c2 >= match_min_pos {
                let back = (m - c2) as usize;
                if win[cur - back] == win[cur] {
                    d[di + 1] = m - c2 - 1;
                    if win[cur - back + 2] == win[cur + 2] {
                        d[di] = 3;
                        return di + 2;
                    }
                    d[di] = 2;
                    di += 2;
                }
            }
            if c3 >= match_min_pos {
                let back = (m - c3) as usize;
                if win[cur - back] == win[cur] {
                    d[di] = 3;
                    d[di + 1] = m - c3 - 1;
                    di += 2;
                }
            }
            return di;
        }

        // C: `MixMatches4`. `BT5_USE_H4` is not defined in the SDK, so there
        // is no fourth hash here.
        if c2 >= match_min_pos {
            let back = (m - c2) as usize;
            if win[cur - back] == win[cur] {
                d[di + 1] = m - c2 - 1;
                if win[cur - back + 2] == win[cur + 2] {
                    if win[cur - back + 3] == win[cur + 3] {
                        d[di] = 4;
                        return di + 2;
                    }
                    d[di] = 3;
                    return di + 2;
                }
                d[di] = 2;
                di += 2;
            }
        }
        if c3 >= match_min_pos {
            let back = (m - c3) as usize;
            if win[cur - back] == win[cur] {
                d[di + 1] = m - c3 - 1;
                if win[cur - back + 3] == win[cur + 3] {
                    d[di] = 4;
                    return di + 2;
                }
                d[di] = 3;
                di += 2;
            }
        }
        di
    }

    /// C: `INCREASE_LZ_POS`.
    #[inline]
    fn increase_lz_pos(&self) {
        // SAFETY: the lz thread owns `lz_state`.
        let l = unsafe { &mut *self.shared().lz_state.get() };
        l.lz_pos += 1;
        l.pointer_to_cur_pos += 1;
    }

    /// C: `MatchFinderMt_GetMatches`, or `MatchFinderMt2_GetMatches` when
    /// there is no low hash to mix in.
    pub(crate) fn get_matches(&mut self, d: &mut [u32]) -> usize {
        let sh = self.shared();
        let c = sh.common();
        // SAFETY: the lz thread owns `lz_state`.
        let l = unsafe { &mut *sh.lz_state.get() };
        // SAFETY: the lz thread holds the current `bt_buf` block until its
        // next `get_next_block`. `bt_pos` below indexes that block.
        let (base, bt) = unsafe { c.bt_held(l.bt_buf_pos) };

        let mut bt_pos = l.bt_buf_pos - base;
        let len = bt[bt_pos];
        bt_pos += 1;

        // Inline stands in for the one-thread finder, so its last few
        // positions follow C `GET_MATCHES_HEADER(minLen)` in LzFind.c: with
        // fewer than `num_hash_bytes` bytes left there are no matches. The
        // threads mix hash matches there, as `MatchFinderMt_GetMatches` does.
        if c.inline && l.bt_num_avail_bytes < c.num_hash_bytes {
            l.bt_num_avail_bytes -= 1;
            l.bt_buf_pos = base + bt_pos + len as usize;
            self.increase_lz_pos();
            return 0;
        }

        if c.mix == Mix::None {
            let bt_lim = bt_pos + len as usize;
            l.bt_buf_pos = base + bt_lim;
            l.bt_num_avail_bytes -= 1;
            self.increase_lz_pos();
            let mut di = 0;
            while bt_pos != bt_lim {
                d[di] = bt[bt_pos];
                d[di + 1] = bt[bt_pos + 1];
                bt_pos += 2;
                di += 2;
            }
            return di;
        }

        let avail = l.bt_num_avail_bytes - 1;
        l.bt_num_avail_bytes = avail;
        l.bt_buf_pos = base + bt_pos + len as usize;

        let mut di = 0;
        if len == 0 {
            if avail >= (BT_HASH_BYTES_MAX - 1) - 1 {
                let mut m = l.lz_pos;
                if m > c.history_size {
                    m -= c.history_size;
                } else {
                    m = 1;
                }
                di = self.mix_matches(m, d, di);
            }
        } else {
            // C: "first match pair from BinTree: (match_len, match_dist)
            // [...] MixMatchesFunc() inserts only hash matches that are nearer
            // than (match_dist)".
            let min_pos = l.lz_pos - bt[bt_pos + 1];
            di = self.mix_matches(min_pos, d, di);
            let mut left = len;
            loop {
                d[di] = bt[bt_pos];
                d[di + 1] = bt[bt_pos + 1];
                bt_pos += 2;
                di += 2;
                left -= 2;
                if left == 0 {
                    break;
                }
            }
        }
        self.increase_lz_pos();
        di
    }

    /// C: `MatchFinderMt0_Skip` / `MatchFinderMt2_Skip` / `MatchFinderMt3_Skip`
    /// over the `SKIP_HEADER_MT` / `SKIP_FOOTER_MT` macro pair. `stream` is
    /// read only inline.
    pub(crate) fn skip(&mut self, stream: &mut dyn SeqInStream, mut num: u32) {
        let min_len = match self.shared().common().mix {
            Mix::None => 0,
            Mix::Two => 2,
            // C: `MatchFinderMt3_Skip` is used for both 4 and 5 hash bytes;
            // "the difference is that MatchFinderMt3_Skip() updates hash for
            // last 3 bytes of stream".
            Mix::Three | Mix::Four => 3,
        };
        loop {
            self.next_block_if_required(stream);
            let sh = self.shared();
            let c = sh.common();
            // SAFETY: the lz thread owns `lz_state` and the low hash, and may
            // read the window.
            let (l, hash, win) = unsafe { (&mut *sh.lz_state.get(), c.low_hash(), c.win()) };
            let avail = l.bt_num_avail_bytes;
            l.bt_num_avail_bytes = avail.wrapping_sub(1);
            if min_len != 0 && avail >= min_len {
                let cur = l.pointer_to_cur_pos;
                let temp = c.crc[usize::from(win[cur])] ^ u32::from(win[cur + 1]);
                let h2 = (temp & (K_HASH2_SIZE - 1)) as usize;
                hash[h2] = l.lz_pos;
                if min_len == 3 {
                    let h3 =
                        ((temp ^ (u32::from(win[cur + 2]) << 8)) & (K_HASH3_SIZE - 1)) as usize;
                    hash[K_FIX3_HASH_SIZE + h3] = l.lz_pos;
                }
            }
            l.lz_pos += 1;
            l.pointer_to_cur_pos += 1;
            // SAFETY: the lz thread holds the block `bt_buf_pos` is in.
            let (base, bt) = unsafe { c.bt_held(l.bt_buf_pos) };
            l.bt_buf_pos += bt[l.bt_buf_pos - base] as usize + 1;
            num -= 1;
            if num == 0 {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Running the threads
// ---------------------------------------------------------------------------

/// The two producer threads, for as long as one stream lasts.
///
/// C: the pair `MatchFinderMt_Create` spawns and `MatchFinderMt_Destruct`
/// stops. They are scoped threads here, so the hash thread can hold the
/// caller's `&mut dyn SeqInStream` directly instead of the C's stored
/// `mf->stream` pointer; [`MtRun`]'s `Drop` is what guarantees they are
/// stopped and joined before that borrow ends.
struct MtRun<'s> {
    sh: Arc<MtShared>,
    hash: Option<std::thread::ScopedJoinHandle<'s, ()>>,
    bt: Option<std::thread::ScopedJoinHandle<'s, ()>>,
}

impl Drop for MtRun<'_> {
    fn drop(&mut self) {
        // C: `MatchFinderMt_Destruct`. "we want thread to be in Stopped state
        // before sending EXIT command. note: stop(btSync) will stop (htSync)
        // also."
        self.sh.bt_sync.stop_writing();
        self.sh.bt_sync.send_exit();
        if let Some(h) = self.bt.take() {
            let _ = h.join();
        }
        self.sh.hash_sync.stop_writing();
        self.sh.hash_sync.send_exit();
        if let Some(h) = self.hash.take() {
            let _ = h.join();
        }
    }
}

/// Runs `body` over one block with the pair [`MatchFinderMt::block_handle`]
/// keeps: the hash thread reads `src` as its input, and the pair is stopped,
/// not ended, when `body` returns.
///
/// C: the window between `MatchFinderMt_Init` and
/// `MatchFinderMt_ReleaseStream`, with the input set by
/// `MatchFinder_SET_DIRECT_INPUT_BUF`. `src` stays borrowed until both
/// producer threads have stopped, on every path out, a panic in `body`
/// included: that is what lets the hash thread hold its address.
///
/// # Errors
///
/// Propagates whatever `body` returns.
pub(crate) fn run_block<T>(
    sh: &MtShared,
    src: &[u8],
    body: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    /// Stops the pair, then forgets the block, when the block ends.
    struct Stop<'a>(&'a MtShared);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            // C: `MatchFinderMt_ReleaseStream`. Waits for the bt thread to
            // stop, and the bt thread stops the hash thread first.
            self.0.bt_sync.stop_writing();
            // SAFETY: both producer threads are stopped, so `block` and
            // `common` are the lz thread's again, and the lz thread holds no
            // borrow of either here.
            unsafe {
                *self.0.block.get() = BlockInput {
                    ptr: core::ptr::null(),
                    len: 0,
                    pos: 0,
                };
                let c = &mut *self.0.common.get();
                if c.direct {
                    c.win = core::ptr::NonNull::dangling().as_ptr();
                    c.win_len = 0;
                }
            }
        }
    }
    // SAFETY: between streams `block` and `common` are the lz thread's: the
    // pair is waiting on `can_start`, which only this thread's first
    // `get_next_block` in `body` sets, and the caller holds no borrow of
    // `common` across this call. `Stop` gives the block back only once both
    // threads have stopped; `a_direct_block_is_read_as_the_copied_window_is`
    // checks the hand-off at both ends, natively and under Miri.
    unsafe {
        *sh.block.get() = BlockInput {
            ptr: src.as_ptr(),
            len: src.len(),
            pos: 0,
        };
        // C: `MatchFinder_SET_DIRECT_INPUT_BUF`, then `MatchFinder_Init`'s
        // `p->buffer = p->bufBase`: the block is the window. The pointer is
        // only ever read through: `read_block` and `move_block`, the two
        // writers, never touch a direct window.
        let c = &mut *sh.common.get();
        if c.direct {
            c.win = src.as_ptr().cast_mut();
            c.win_len = src.len();
        }
    }
    let stop = Stop(sh);
    let r = body();
    drop(stop);
    r
}

/// Runs `body` with the hash and bt threads live.
///
/// C: the window between `MatchFinderMt_Create` and
/// `MatchFinderMt_ReleaseStream` - one stream's worth of encoding. The threads
/// are spawned here rather than once per encoder because that is what lets the
/// hash thread borrow `stream`; one pair of spawns per LZMA2 block, which is
/// at least a megabyte of input, is not measurable.
///
/// # Errors
///
/// Propagates whatever `body` returns.
pub(crate) fn with_threads<T>(
    sh: &Arc<MtShared>,
    stream: &mut (dyn SeqInStream + Send),
    body: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    std::thread::scope(|scope| {
        let run = {
            let hash_sh = Arc::clone(sh);
            let bt_sh = Arc::clone(sh);
            // Set before the spawns: the bt thread calls
            // `hash_sync.stop_writing()`, which does nothing unless the flag is
            // already up.
            MtSync::set(&sh.hash_sync.was_created, true);
            MtSync::set(&sh.bt_sync.was_created, true);
            let hash = scope.spawn(move || hash_thread_func(&hash_sh, stream));
            let bt = scope.spawn(move || bt_thread_func(&bt_sh));
            MtRun {
                sh: Arc::clone(sh),
                hash: Some(hash),
                bt: Some(bt),
            }
        };
        let r = body();
        // C: `MatchFinderMt_ReleaseStream`, before the threads are torn down.
        run.sh.bt_sync.stop_writing();
        drop(run);
        r
    })
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    use alloc::vec;
    use alloc::vec::Vec;

    use super::{
        BT_BLOCK_SIZE, BT_BUFFER_SIZE, HASH_BLOCK_SIZE, HASH_BUFFER_SIZE, MatchFinder,
        MatchFinderMt, get_matches_spec_n_2, get_matches_spec_n_2_checked, run_block, with_threads,
    };
    use crate::enc::consts::{K_NUM_OPTS, LZMA_MATCH_LEN_MAX};
    use crate::enc::lz_find::MatchFinderKind;
    use crate::enc::stream::{NoStream, SeqInStream, SliceStream};
    use crate::enc::{Lzma2Encoder, LzmaEncProps};
    use crate::error::Error;

    /// The `cyclicBufferSize` whose bt thread panics on purpose. Zero, the
    /// default, matches nothing: a cyclic buffer is never empty. Keyed on a
    /// size no other test uses, so that tests running alongside are not hit.
    static BT_PANIC_AT_CBS: AtomicU32 = AtomicU32::new(0);

    pub(super) fn maybe_panic_in_bt(cyclic_buffer_size: u32) {
        let at = BT_PANIC_AT_CBS.load(Ordering::SeqCst);
        if at != 0 && at == cyclic_buffer_size {
            panic!("injected panic on the bt thread");
        }
    }

    /// A panic on the bt thread must come back to the caller as an error.
    /// Before the bt thread caught it, the lz thread waited for a block the
    /// dead thread would never fill, and the encode never returned.
    #[test]
    fn a_panic_on_the_bt_thread_returns_an_error() {
        // A dictionary size nothing else in the suite asks for.
        const DICT: u32 = 0x0001_2345;
        BT_PANIC_AT_CBS.store(DICT + 1, Ordering::SeqCst);

        let src: alloc::vec::Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let props = LzmaEncProps::new()
            .with_level(5)
            .with_dict_size(DICT)
            .with_num_threads(2);
        let mut enc = Lzma2Encoder::new(&props).expect("encoder");
        let mut out = alloc::vec::Vec::new();
        let r = enc.encode_send(&mut SliceStream::new(&src), &mut out);
        BT_PANIC_AT_CBS.store(0, Ordering::SeqCst);
        assert!(r.is_err(), "a bt-thread panic was reported as success");
    }

    /// Hands out `ok` bytes in one read, then panics on the next one.
    struct PanicAfter<'a> {
        data: &'a [u8],
        given: bool,
    }

    impl SeqInStream for PanicAfter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
            if self.given {
                panic!("injected panic in the input stream");
            }
            assert!(
                buf.len() >= self.data.len(),
                "window smaller than the test input"
            );
            buf[..self.data.len()].copy_from_slice(self.data);
            self.given = true;
            Ok(self.data.len())
        }
    }

    /// A panic in the input stream must not take back bytes the lz thread
    /// was already promised.
    ///
    /// The first bt block's header promises every byte the hash thread had
    /// read, and the encoder is entitled to consume that many positions -
    /// `GetOptimum` looks ahead and `Skip`s on that count. The stream panics
    /// on its second read, when some of those bytes are not hashed yet. The
    /// hash thread used to hand the bt thread an orderly end of stream with
    /// no bytes left, so the bt blocks stopped short of the promise and the
    /// lz thread walked past the last published entry into words no thread
    /// wrote this run: an out-of-bounds index on the encoder's match array,
    /// seen on a Windows runner. The ring is poisoned first, so any such read
    /// fails here on every platform and at every interleaving: what the bt
    /// thread publishes depends only on the input, and the lz side consumes
    /// the whole promise whatever the threads' timing.
    #[test]
    fn a_stream_panic_never_strands_the_lz_thread_past_the_published_blocks() {
        let src: alloc::vec::Vec<u8> = (0..600_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8 % 17)
            .collect();
        let mut mt = MatchFinderMt::new();
        mt.mfb.kind = MatchFinderKind::Bt4;
        mt.mfb.num_hash_bytes = 4;
        mt.mfb.cut_value = 32;
        mt.create(
            1 << 16,
            K_NUM_OPTS as u32,
            273,
            LZMA_MATCH_LEN_MAX + 1,
            u64::MAX,
            false,
            false,
        )
        .expect("create");
        mt.init_mt().expect("init_mt");
        mt.init();
        // SAFETY: neither producer thread has been started.
        unsafe { mt.shared().common().bt_all() }.fill(u32::MAX);

        let sh = mt.stream_handle().expect("created");
        let mut input = PanicAfter {
            data: &src,
            given: false,
        };
        let r = with_threads(&sh, &mut input, || {
            let promised = mt.get_num_available_bytes(&mut NoStream);
            assert_eq!(promised as usize, src.len(), "first block's promise");
            mt.skip(&mut NoStream, promised);
            Ok(mt.failed())
        });
        assert_eq!(r, Ok(true), "the stream panic was not reported");
    }

    /// A small generator with a fixed seed: the cases are the same on every
    /// run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
        }

        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
    }

    /// One call's arguments.
    struct Case {
        win: Vec<u8>,
        len_limit_0: usize,
        pos: u32,
        cur: usize,
        son: Vec<u32>,
        cut: u32,
        d_len: usize,
        di: usize,
        max_len_0: usize,
        heads: Vec<u32>,
        hi: usize,
        limit: usize,
        hsize: usize,
        cbp: u32,
        cyclic: u32,
    }

    /// Arguments that meet the kernel's five conditions and are otherwise as
    /// hostile as the generator can make them: cyclic buffers down to one
    /// slot, windows that end exactly where the run does, positions at the
    /// start of a stream, just normalised and about to wrap, heads that are
    /// zero or far out of range, and a tree full of zeros and noise.
    fn case(rng: &mut Rng) -> Case {
        const CYCLIC: [u32; 12] = [1, 2, 3, 4, 5, 7, 8, 16, 33, 64, 257, 1024];
        let cyclic = CYCLIC[rng.below(12) as usize];
        let n = 1 + rng.below(cyclic.min(48)) as usize;
        let cbp = rng.below(cyclic - n as u32 + 1);
        let max_len_0 = 1 + rng.below(4) as usize;
        let len_limit_c = max_len_0 + 1 + rng.below(40) as usize;
        let cur = rng.below(600) as usize;
        let deep = cyclic as usize <= cur + 1;
        let pos = match rng.below(4) {
            // Just normalised.
            1 if deep => cyclic,
            // About to wrap, or wrapping inside the run.
            2 if deep => u32::MAX - rng.below(n as u32 + 2),
            3 if deep => cyclic + rng.below(1 << 20),
            // The start of a stream: no further on than the window is.
            _ => 1 + rng.below(cur as u32 + 1),
        };
        let len_limit_0 = cur + len_limit_c - 1;
        let win_len = len_limit_0 + n + rng.below(3) as usize;
        let alphabet = [1, 2, 3, 250][rng.below(4) as usize];
        let win = (0..win_len).map(|_| rng.below(alphabet) as u8).collect();
        let sons = 2 * cyclic as usize + rng.below(2) as usize;
        let son = (0..sons)
            .map(|_| match rng.below(8) {
                0 => 0,
                1 => rng.next(),
                _ => pos.wrapping_sub(rng.below(2 * cyclic + 4)),
            })
            .collect();
        let hi = rng.below(3) as usize;
        let hsize = hi + n;
        let mut heads = Vec::new();
        let mut last = 1;
        for _ in 0..hsize + rng.below(2) as usize {
            // Runs of one distance are what the long-match path feeds on.
            last = match rng.below(64) {
                0 => 0,
                1 => rng.next(),
                2..=39 => last.max(1),
                _ => 1 + rng.below(cyclic + 2),
            };
            heads.push(last);
        }
        let di = rng.below(4) as usize;
        let d_len = di + n * (3 + 2 * len_limit_c) + 8;
        let limit = if rng.below(2) == 0 {
            d_len
        } else {
            di + 1 + rng.below(3 * n as u32 + 1) as usize
        };
        Case {
            win,
            len_limit_0,
            pos,
            cur,
            son,
            cut: 1 + rng.below(cyclic + 3),
            d_len,
            di,
            max_len_0,
            heads,
            hi,
            limit,
            hsize,
            cbp,
            cyclic,
        }
    }

    type Kernel = fn(
        &[u8],
        usize,
        u32,
        usize,
        &mut [u32],
        u32,
        &mut [u32],
        usize,
        usize,
        &[u32],
        usize,
        usize,
        usize,
        u32,
        u32,
    ) -> Option<(usize, u32)>;

    /// What `kernel` answers for `c`, and the tree and matches it leaves.
    fn walk(c: &Case, kernel: Kernel) -> (Option<(usize, u32)>, Vec<u32>, Vec<u32>) {
        let mut son = c.son.clone();
        let mut d = vec![0xDEAD_BEEF_u32; c.d_len];
        let got = kernel(
            &c.win,
            c.len_limit_0,
            c.pos,
            c.cur,
            &mut son,
            c.cut,
            &mut d,
            c.di,
            c.max_len_0,
            &c.heads,
            c.hi,
            c.limit,
            c.hsize,
            c.cbp,
            c.cyclic,
        );
        (got, son, d)
    }

    /// Generated tables the differential test below walks. Under Miri, which
    /// runs the CI job that checks every unchecked access for undefined
    /// behaviour, a thousand: the same generator and the same seed, so
    /// they are the first cases of the full run, one-slot cyclic buffers and
    /// the normalisation and wrap boundaries among them.
    const WALK_CASES: u32 = if cfg!(miri) { 1_000 } else { 60_000 };

    /// The walk with its per-node accesses unchecked must be the walk with
    /// every access checked: the same answer, the same tree, the same
    /// matches, whatever the window and the tables hold. Built with debug
    /// assertions, as tests are, every unchecked access here is also checked
    /// against its slice, so a case that stepped outside would fail this
    /// rather than pass it by luck; under Miri, an access outside its slice
    /// is reported as undefined behaviour whatever the build.
    #[test]
    fn the_unchecked_walk_is_the_checked_walk_on_any_tables() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let (mut walked, mut refused) = (0_u32, 0_u32);
        for case_no in 0..WALK_CASES {
            let c = case(&mut rng);
            let want = walk(&c, get_matches_spec_n_2_checked);
            let got = walk(&c, get_matches_spec_n_2);
            assert!(got == want, "case {case_no}");
            if want.0.is_some() {
                walked += 1;
            } else {
                refused += 1;
            }
        }
        // Both of the kernel's answers are well represented.
        assert!(
            walked > WALK_CASES / 30 && refused > WALK_CASES / 30,
            "{walked} walked, {refused} refused"
        );
    }

    /// A call that breaks any one of the five conditions is refused before
    /// it reads or writes anything: it panics, which the bt thread turns
    /// into a failed stream.
    #[test]
    fn the_walk_refuses_what_it_cannot_vouch_for() {
        fn base() -> Case {
            let mut rng = Rng(11);
            loop {
                let c = case(&mut rng);
                if c.cyclic >= 16 && c.hsize - c.hi >= 2 {
                    return c;
                }
            }
        }
        type Break = (&'static str, fn(&mut Case));
        let breaks: [Break; 6] = [
            ("heads", |c| c.heads.truncate(c.hsize - 1)),
            ("A", |c| c.cbp = c.cyclic - (c.hsize - c.hi) as u32 + 1),
            ("B", |c| c.son.truncate(2 * c.cyclic as usize - 1)),
            ("C", |c| c.max_len_0 = c.len_limit_0 - c.cur + 1),
            ("D", |c| {
                c.win.truncate(c.len_limit_0 + (c.hsize - c.hi) - 1)
            }),
            ("E", |c| {
                // A position past the window's start with a cyclic buffer
                // that reaches back further than the window does.
                c.cyclic = (c.cur + 2 + (c.hsize - c.hi)) as u32;
                c.son = vec![0; 2 * c.cyclic as usize];
                c.cbp = 0;
                c.pos = c.cur as u32 + 2;
            }),
        ];
        // The base case itself is accepted.
        let c = base();
        assert_eq!(
            walk(&c, get_matches_spec_n_2),
            walk(&c, get_matches_spec_n_2_checked)
        );
        for (name, break_it) in breaks {
            let mut c = base();
            break_it(&mut c);
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                walk(&c, get_matches_spec_n_2)
            }));
            assert!(refused.is_err(), "condition {name} was not tested");
        }
    }

    /// Bytes with long and short repeats among noise, so that the finder
    /// reports matches of every length and none.
    fn block_bytes(rng: &mut Rng, len: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(len);
        while v.len() < len {
            let run = 1 + rng.below(300) as usize;
            if v.len() > 8 && rng.below(3) != 0 {
                let back = 1 + rng.below(v.len().min(70_000) as u32) as usize;
                for _ in 0..run {
                    v.push(v[v.len() - back]);
                }
            } else {
                let alphabet = [2, 5, 256][rng.below(3) as usize];
                for _ in 0..run {
                    v.push(rng.below(alphabet) as u8);
                }
            }
        }
        v.truncate(len);
        v
    }

    /// The calls the encoder makes on one block - `GetNumAvailableBytes`,
    /// then `GetMatches` or a `Skip` within what is available - for at most
    /// `calls` positions, everything the finder answers written to `trace`.
    /// With `panics` set it ends in a panic, as a coder that failed would.
    /// `stream` is what an inline finder reads; the threaded one ignores it.
    fn drive(
        mt: &mut MatchFinderMt,
        stream: &mut dyn SeqInStream,
        seed: u64,
        calls: usize,
        panics: bool,
        trace: &mut Vec<u32>,
    ) {
        let mut rng = Rng(seed);
        let mut d = vec![0u32; (LZMA_MATCH_LEN_MAX * 2 + 2) as usize];
        for _ in 0..calls {
            let avail = mt.get_num_available_bytes(stream);
            // What a coder can see of it: the one-thread finder keeps
            // `match_max_len + LZMA_MATCH_LEN_MAX + 1` bytes ahead (C
            // `keepSizeAfter`), and no look-ahead limit in the coder exceeds
            // that; past it each finder reads the stream in its own steps.
            trace.push(avail.min(2 * LZMA_MATCH_LEN_MAX + 1));
            if avail == 0 {
                break;
            }
            if rng.below(4) == 0 {
                let n = 1 + rng.below(avail.min(40));
                mt.skip(stream, n);
                trace.push(u32::MAX - n);
            } else {
                let k = mt.get_matches(&mut d);
                trace.push(k as u32);
                trace.extend_from_slice(&d[..k]);
            }
            trace.push(u32::from(mt.window()[mt.cur() - 1]));
        }
        assert!(!panics, "a coder that fails partway through its block");
    }

    /// [`drive`] over the one-thread finder: the same calls from the same
    /// seed, and the same trace.
    fn drive_st(mf: &mut MatchFinder, stream: &mut dyn SeqInStream, seed: u64) -> Vec<u32> {
        let mut rng = Rng(seed);
        let mut d = vec![0u32; (LZMA_MATCH_LEN_MAX * 2 + 2) as usize];
        let mut trace = Vec::new();
        loop {
            let avail = mf.get_num_available_bytes();
            trace.push(avail.min(2 * LZMA_MATCH_LEN_MAX + 1));
            if avail == 0 {
                return trace;
            }
            if rng.below(4) == 0 {
                let n = 1 + rng.below(avail.min(40));
                mf.skip(stream, n);
                trace.push(u32::MAX - n);
            } else {
                let k = mf.get_matches(stream, &mut d);
                trace.push(k as u32);
                trace.extend_from_slice(&d[..k]);
            }
            trace.push(u32::from(mf.buf_base[mf.cur() - 1]));
        }
    }

    /// `create`, `InitMt` and `Init` for one block of `len` bytes, the window
    /// cut to it as `encode_slice` cuts it, or no window with `direct`;
    /// with no threads at all with `inline`.
    fn prepare(
        mt: &mut MatchFinderMt,
        kind: MatchFinderKind,
        len: usize,
        direct: bool,
        inline: bool,
    ) {
        mt.mfb.kind = kind;
        mt.mfb.num_hash_bytes = match kind {
            MatchFinderKind::Bt2 => 2,
            MatchFinderKind::Bt3 => 3,
            MatchFinderKind::Bt4 => 4,
            _ => 5,
        };
        mt.mfb.cut_value = 32;
        mt.mfb.expected_data_size = len as u64;
        // Under Miri the tables are kept small: every stream fills them.
        let dict = if cfg!(miri) { 1 << 12 } else { 1 << 16 };
        mt.create(
            dict,
            K_NUM_OPTS as u32,
            LZMA_MATCH_LEN_MAX,
            LZMA_MATCH_LEN_MAX + 1,
            len as u64,
            direct,
            inline,
        )
        .expect("create");
        mt.init_mt().expect("init_mt");
        mt.init();
    }

    /// The direct-input path is `run_block` handing the kept producer threads
    /// a raw pointer to the caller's block, and taking it back in `Stop` once
    /// both have stopped. It must find, block after block, what the copying
    /// path finds - `with_threads` reading the same bytes into the finder's
    /// own window - and the encoder must write the same bytes through it.
    ///
    /// Each block is a fresh allocation, dropped as soon as `run_block`
    /// returns, so under Miri a producer thread that read the block after
    /// `Stop` is a use after free, and one that raced the hand-off at either
    /// end is a data race. The schedule ends blocks at every point the lz
    /// thread can: before the threads are started at all, just after the
    /// first hand-off, partway through, at the end, and in a panic, after
    /// which the kept pair must take the next block as if nothing happened.
    /// Which of the producers' steps each of those meets is left to the
    /// scheduler, natively and under Miri's.
    #[test]
    fn a_direct_block_is_read_as_the_copied_window_is() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let (len, kinds): (usize, &[MatchFinderKind]) = if cfg!(miri) {
            (300, &[MatchFinderKind::Bt4])
        } else {
            (
                300_000,
                &[
                    MatchFinderKind::Bt2,
                    MatchFinderKind::Bt3,
                    MatchFinderKind::Bt4,
                    MatchFinderKind::Bt5,
                ],
            )
        };
        // (block length, calls before the block ends, whether it panics).
        // Under Miri, one block for each way a block can end, with a full
        // block after the panic to show the kept pair still works.
        let full: &[(usize, usize, bool)] = &[
            (len, usize::MAX, false),
            (len, 0, false),
            (len, 1, false),
            (len / 3 + 7, 37, false),
            (len, usize::MAX, false),
            (len / 2, 61, true),
            (1, usize::MAX, false),
            (0, usize::MAX, false),
            (len / 5 + 3, usize::MAX, false),
        ];
        let small: &[(usize, usize, bool)] = &[
            (len, 0, false),
            (len / 3 + 7, 37, false),
            (len / 2, 61, true),
            (len, usize::MAX, false),
        ];
        let schedule = if cfg!(miri) { small } else { full };

        let mut rng = Rng(0xD1EC_7B10_C4ED);
        let mut direct = MatchFinderMt::new();
        let mut copied = MatchFinderMt::new();
        for &kind in kinds {
            for (i, &(len, calls, panics)) in schedule.iter().enumerate() {
                let src = block_bytes(&mut rng, len);
                let seed = u64::from(rng.next()) | 1;
                let what = format!("{kind:?}, block {i}");

                prepare(&mut copied, kind, len, false, false);
                let sh = copied.stream_handle().expect("created");
                let mut want = Vec::new();
                let mut input = SliceStream::new(&src);
                let copied_run = catch_unwind(AssertUnwindSafe(|| {
                    with_threads(&sh, &mut input, || {
                        drive(&mut copied, &mut NoStream, seed, calls, panics, &mut want);
                        Ok(())
                    })
                }));

                prepare(&mut direct, kind, len, true, false);
                let sh = direct.block_handle().expect("spawn").expect("created");
                let mut got = Vec::new();
                let block = src.clone();
                let direct_run = catch_unwind(AssertUnwindSafe(|| {
                    run_block(&sh, &block, || {
                        drive(&mut direct, &mut NoStream, seed, calls, panics, &mut got);
                        Ok(())
                    })
                }));
                drop(block);

                assert_eq!(copied_run.is_err(), panics, "{what}");
                assert_eq!(direct_run.is_err(), panics, "{what}");
                if !panics {
                    assert_eq!(copied_run.unwrap(), Ok(()), "{what}");
                    assert_eq!(direct_run.unwrap(), Ok(()), "{what}");
                }
                assert!(got == want, "{what}: the direct block found other matches");
                if calls == usize::MAX {
                    assert_eq!(
                        want.last(),
                        Some(&0),
                        "{what}: the block was not read to its end"
                    );
                }
                assert_eq!(direct.windows(), (0, 0), "{what}: the block is still held");
                assert_eq!(direct.spawns, 1, "{what}: one pair for every block");
            }
        }

        // And the encoder: block coders that read their blocks in place write
        // what the stream path, which copies them into the window, writes.
        let (len, block) = if cfg!(miri) {
            (600, 300)
        } else {
            (400_000, 1 << 16)
        };
        let src = block_bytes(&mut rng, len);
        let props = LzmaEncProps::new()
            .with_level(5)
            .with_dict_size(1 << 12)
            .with_num_threads(2);
        let mut in_place = Lzma2Encoder::new(&props).unwrap();
        in_place.set_block_size(block);
        in_place.set_threads(2);
        in_place.set_data_size(len as u64);
        let mut got = Vec::new();
        in_place.encode_slice(&src, &mut got).unwrap();
        let mut copying = Lzma2Encoder::new(&props).unwrap();
        copying.set_block_size(block);
        copying.set_data_size(len as u64);
        let mut want = Vec::new();
        copying
            .encode_send(&mut SliceStream::new(&src), &mut want)
            .unwrap();
        assert!(got == want, "reading blocks in place changed the output");
    }

    /// The inline finder runs the hash and bt stages on the caller's thread a
    /// block at a time, and stands in for the one-thread finder, so it must
    /// answer every call as that finder does: through its own window, read from the stream it is handed and
    /// slid as the stream runs past it, and reading a block in place through
    /// `run_block`, where no thread is started.
    ///
    /// Natively the window slides many times over each input (a small
    /// dictionary, no data limit), and the high-hash and son tables wrap.
    /// Under Miri the inputs are small; what it checks is the inline path's
    /// borrows of the shared state, which the stages now take in turn on one
    /// thread.
    #[test]
    fn the_inline_finder_finds_what_the_one_thread_finder_finds() {
        // `prepare`'s dictionary, which the direct block is set up with.
        let (lens, kinds, dict): (&[usize], &[MatchFinderKind], u32) = if cfg!(miri) {
            (&[0, 1, 700], &[MatchFinderKind::Bt4], 1 << 12)
        } else {
            (
                &[0, 1, 5, 4_000, 300_000, 1_500_000],
                &[
                    MatchFinderKind::Bt2,
                    MatchFinderKind::Bt3,
                    MatchFinderKind::Bt4,
                    MatchFinderKind::Bt5,
                ],
                1 << 16,
            )
        };
        let setup = |mt: &mut MatchFinderMt, kind: MatchFinderKind, inline: bool| {
            mt.mfb.kind = kind;
            mt.mfb.num_hash_bytes = kind.num_hash_bytes();
            mt.mfb.cut_value = 32;
            mt.create(
                dict,
                K_NUM_OPTS as u32,
                LZMA_MATCH_LEN_MAX,
                LZMA_MATCH_LEN_MAX + 1,
                u64::MAX,
                false,
                inline,
            )
            .expect("create");
            mt.init_mt().expect("init_mt");
            mt.init();
        };

        let mut rng = Rng(0x1A1E_F1D0_5EED);
        let mut inline = MatchFinderMt::new();
        let mut direct = MatchFinderMt::new();
        for &kind in kinds {
            for &len in lens {
                let src = block_bytes(&mut rng, len);
                let seed = u64::from(rng.next()) | 1;
                let what = format!("{kind:?}, {len} bytes");

                let mut st = MatchFinder::new();
                st.kind = kind;
                st.num_hash_bytes = kind.num_hash_bytes();
                st.cut_value = 32;
                st.create(
                    dict,
                    K_NUM_OPTS as u32,
                    LZMA_MATCH_LEN_MAX,
                    LZMA_MATCH_LEN_MAX + 1,
                    u64::MAX,
                )
                .expect("create");
                let mut stream = SliceStream::new(&src);
                st.init(&mut stream);
                let want = drive_st(&mut st, &mut stream, seed);

                setup(&mut inline, kind, true);
                assert!(inline.is_inline(), "{what}");
                assert!(inline.stream_handle().is_none(), "{what}: no threads");
                let mut got = Vec::new();
                drive(
                    &mut inline,
                    &mut SliceStream::new(&src),
                    seed,
                    usize::MAX,
                    false,
                    &mut got,
                );
                assert!(!inline.failed(), "{what}");
                assert!(got == want, "{what}: the inline finder found other matches");

                prepare(&mut direct, kind, len, true, true);
                let sh = direct.block_handle().expect("no spawn").expect("created");
                let mut got = Vec::new();
                run_block(&sh, &src, || {
                    drive(
                        &mut direct,
                        &mut NoStream,
                        seed,
                        usize::MAX,
                        false,
                        &mut got,
                    );
                    Ok(())
                })
                .expect("direct");
                drop(sh);
                assert_eq!(
                    direct.spawns, 0,
                    "{what}: the inline finder started a thread"
                );
                assert_eq!(direct.windows(), (0, 0), "{what}: the block is still held");
                assert!(
                    got == want,
                    "{what}: the inline finder read the block otherwise"
                );
            }
        }
    }

    /// The inline finder holds one hash block and one bt block, not the
    /// threaded finder's rings, and its window keeps one bt block more than
    /// the single-threaded finder's rather than both rings.
    #[test]
    fn the_inline_finder_holds_one_block_of_each_buffer() {
        let dict = 1u32 << 16;
        let make = |inline: bool| {
            let mut mt = MatchFinderMt::new();
            mt.mfb.kind = MatchFinderKind::Bt4;
            mt.mfb.num_hash_bytes = 4;
            mt.mfb.cut_value = 32;
            mt.create(
                dict,
                K_NUM_OPTS as u32,
                LZMA_MATCH_LEN_MAX,
                LZMA_MATCH_LEN_MAX + 1,
                u64::MAX,
                false,
                inline,
            )
            .expect("create");
            mt
        };
        let threaded = make(false);
        let inline = make(true);
        // SAFETY: no thread has been started.
        let words = |mt: &MatchFinderMt| unsafe { (*mt.shared().own.get()).bufs.len() };
        assert_eq!(words(&threaded), HASH_BUFFER_SIZE + BT_BUFFER_SIZE + 2);
        assert_eq!(
            words(&inline),
            (HASH_BLOCK_SIZE + BT_BLOCK_SIZE) as usize + 2
        );
        let (inline_win, _) = inline.windows();
        let (threaded_win, _) = threaded.windows();
        assert!(inline_win < threaded_win, "{inline_win} {threaded_win}");
        let mut est = MatchFinder::new();
        est.kind = MatchFinderKind::Bt4;
        est.num_hash_bytes = 4;
        est.cut_value = 32;
        let estimate = MatchFinderMt::mem_usage(
            &mut est,
            dict,
            K_NUM_OPTS as u32,
            LZMA_MATCH_LEN_MAX,
            LZMA_MATCH_LEN_MAX + 1,
            false,
            true,
        )
        .expect("estimate");
        assert!(
            estimate >= inline.allocated(),
            "{estimate} {}",
            inline.allocated()
        );
    }

    /// Every coder held to one thread now takes the inline finder, and must
    /// write the single-threaded finder's bytes: LZMA through a stream, LZMA2
    /// through a stream, and LZMA2 block coders reading their blocks in
    /// place. Random windows of a mixed input at random settings, plus the
    /// shapes that sit on the buffer edges.
    #[test]
    fn one_thread_writes_the_single_threaded_finders_bytes() {
        use crate::enc::LzmaEncoder;

        let mut rng = Rng(0x0E7E_1A7E_B17E);
        let pool = block_bytes(&mut rng, 3 << 20);
        let mut cases: Vec<(String, Vec<u8>)> = vec![
            ("empty".into(), Vec::new()),
            ("one".into(), vec![0x2A]),
            ("zeros".into(), vec![0; 300_001]),
            ("pool".into(), pool.clone()),
        ];
        let windows = if cfg!(miri) { 0 } else { 24 };
        for i in 0..windows {
            let len = match i % 4 {
                0 => 1 + rng.below(300) as usize,
                1 => 60_000 + rng.below(20_000) as usize,
                2 => 200_000 + rng.below(400_000) as usize,
                _ => 1_000_000 + rng.below(1 << 20) as usize,
            };
            let at = rng.below((pool.len() - len) as u32) as usize;
            cases.push((format!("window {i} at {at}"), pool[at..at + len].to_vec()));
        }
        let kinds = [
            MatchFinderKind::Bt2,
            MatchFinderKind::Bt3,
            MatchFinderKind::Bt4,
            MatchFinderKind::Bt5,
        ];
        for (i, (name, src)) in cases.iter().enumerate() {
            let kind = kinds[i % kinds.len()];
            let level = 5 + rng.below(5);
            let dict = 1u32 << (12 + rng.below(9));
            let fb = [5u32, 32, 64, 273][rng.below(4) as usize];
            let props = LzmaEncProps::new()
                .with_level(level)
                .with_dict_size(dict)
                .with_match_finder(kind)
                .with_fast_bytes(fb)
                .with_num_threads(1);
            let what = format!("{name}: {kind:?} level {level} dict {dict} fb {fb}");

            // LZMA through a stream.
            let mut st = LzmaEncoder::new(&props).unwrap();
            st.inner.inline_finder = false;
            let mut want = Vec::new();
            st.encode(&mut SliceStream::new(src), &mut want).unwrap();
            assert!(!st.inner.mf.is_inline(), "{what}");
            let mut inline = LzmaEncoder::new(&props).unwrap();
            let mut got = Vec::new();
            inline.encode(&mut SliceStream::new(src), &mut got).unwrap();
            assert!(inline.inner.mf.is_inline(), "{what}");
            assert!(got == want, "{what}: LZMA");

            // LZMA2 through a stream, in blocks, and the same blocks coded
            // in place on block threads.
            let block = [1u64 << 16, 1 << 18, 3 << 19][rng.below(3) as usize];
            let mut st = Lzma2Encoder::new(&props).unwrap();
            st.set_block_size(block);
            st.sync_coder().unwrap();
            st.coder.set_inline_finder(false);
            let mut want = Vec::new();
            st.encode(&mut SliceStream::new(src), &mut want).unwrap();
            assert!(!st.coder.finder_is_inline(), "{what}");

            let mut inline = Lzma2Encoder::new(&props).unwrap();
            inline.set_block_size(block);
            let mut got = Vec::new();
            inline.encode(&mut SliceStream::new(src), &mut got).unwrap();
            assert!(inline.coder.finder_is_inline(), "{what}");
            assert!(got == want, "{what}: LZMA2 stream, block {block}");

            let mut blocks = Lzma2Encoder::new(&props).unwrap();
            blocks.set_block_size(block);
            blocks.set_threads(3);
            let mut got = Vec::new();
            blocks.encode_slice(src, &mut got).unwrap();
            assert!(got == want, "{what}: LZMA2 blocks in place, block {block}");
        }
    }

    /// The hash-chain finder taking its heads a run of positions ahead finds
    /// what it found looking each up as it got there, through a window that
    /// slides many times, at the end of the stream, and with skips of every
    /// length between the calls; and an encoder writes the same bytes with
    /// it at every hash-chain level.
    #[test]
    fn hash_chain_heads_taken_ahead_find_the_same_matches() {
        let mut rng = Rng(0x4EAD_5EED);
        for kind in [MatchFinderKind::Hc4, MatchFinderKind::Hc5] {
            for len in [0usize, 1, 4, 5, 6, 4_000, 300_000, 1_500_000] {
                let src = block_bytes(&mut rng, len);
                let seed = u64::from(rng.next()) | 1;
                let mut traces = Vec::new();
                for ahead in [false, true] {
                    let mut mf = MatchFinder::new();
                    mf.kind = kind;
                    mf.num_hash_bytes = kind.num_hash_bytes();
                    mf.cut_value = 16;
                    mf.hc_heads = ahead;
                    mf.create(
                        1 << 16,
                        K_NUM_OPTS as u32,
                        64,
                        LZMA_MATCH_LEN_MAX + 1,
                        u64::MAX,
                    )
                    .expect("create");
                    let mut stream = SliceStream::new(&src);
                    mf.init(&mut stream);
                    traces.push(drive_st(&mut mf, &mut stream, seed));
                }
                assert!(traces[0] == traces[1], "{kind:?}, {len} bytes");
            }
        }

        let pool = block_bytes(&mut rng, 3 << 20);
        for case in 0..16 {
            let at = rng.below((pool.len() - (1 << 20)) as u32) as usize;
            let src = &pool[at..at + rng.below(1 << 20) as usize];
            let level = rng.below(5);
            let dict = 1u32 << (12 + rng.below(9));
            let props = LzmaEncProps::new().with_level(level).with_dict_size(dict);
            let mut out = Vec::new();
            for ahead in [false, true] {
                let mut enc = crate::enc::LzmaEncoder::new(&props).unwrap();
                enc.inner.mf.cfg().hc_heads = ahead;
                let mut bytes = Vec::new();
                enc.encode(&mut SliceStream::new(src), &mut bytes).unwrap();
                assert!(
                    !enc.inner.mf.cfg().kind.bt_mode(),
                    "level {level} is a hash chain"
                );
                out.push(bytes);
            }
            assert!(
                out[0] == out[1],
                "case {case}: level {level} dict {dict}, {} bytes",
                src.len()
            );
        }
    }
}
