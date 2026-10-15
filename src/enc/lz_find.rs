//! The match finders.
//!
//! C: `C/LzFind.c` and `C/LzFind.h` — `CMatchFinder` with the hash-chain
//! (`hc4`, `hc5`) and binary-tree (`bt2`, `bt3`, `bt4`, `bt5`) variants that
//! `LzmaEnc` configures. `C/LzHash.h` supplies the hash constants.
//!
//! The window is `buf_base`, an owned byte buffer, and `buffer` is an offset into it
//! rather than the C's pointer; `hash` holds the hash table and the `son`
//! table in one allocation exactly as `MatchFinder_Create` does, with `son`
//! starting at `son_base`.
//!
//! `LzFindMt.c`, which drives these same tables from two more threads, is in
//! `super::lz_find_mt`.
//!
//! Not ported: `Bt3Zip_*`/`Hc3Zip_*` (the Deflate-shaped finders, which
//! `LzmaEnc` never selects). The normalization kernel family is in `normalize`.

use alloc::vec::Vec;

use crate::enc::consts::*;
use crate::enc::match_run::match_run;
use crate::enc::stream::SeqInStream;
use crate::error::Error;

/// Which match finder to use.
///
/// C: the `btMode` / `numHashBytes` pair that `MatchFinder_CreateVTable`
/// switches on, as one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchFinderKind {
    /// C: `Hc4_MatchFinder_GetMatches`. Hash chain, 4-byte hash.
    Hc4,
    /// C: `Hc5_MatchFinder_GetMatches`. Hash chain, 5-byte hash.
    Hc5,
    /// C: `Bt2_MatchFinder_GetMatches`. Binary tree, 2-byte hash.
    Bt2,
    /// C: `Bt3_MatchFinder_GetMatches`. Binary tree, 3-byte hash.
    Bt3,
    /// C: `Bt4_MatchFinder_GetMatches`. Binary tree, 4-byte hash.
    Bt4,
    /// C: `Bt5_MatchFinder_GetMatches`. Binary tree, 5-byte hash.
    Bt5,
}

impl MatchFinderKind {
    /// C: `MFB.btMode`.
    #[must_use]
    pub const fn bt_mode(self) -> bool {
        !matches!(self, MatchFinderKind::Hc4 | MatchFinderKind::Hc5)
    }

    /// C: `MFB.numHashBytes`.
    #[must_use]
    pub const fn num_hash_bytes(self) -> u32 {
        match self {
            MatchFinderKind::Bt2 => 2,
            MatchFinderKind::Bt3 => 3,
            MatchFinderKind::Hc4 | MatchFinderKind::Bt4 => 4,
            MatchFinderKind::Hc5 | MatchFinderKind::Bt5 => 5,
        }
    }

    /// The pair `LzmaEnc_SetProps` derives from `btMode` and `numHashBytes`.
    pub(crate) fn from_props(bt_mode: bool, num_hash_bytes: u32) -> Self {
        if !bt_mode {
            if num_hash_bytes <= 4 {
                MatchFinderKind::Hc4
            } else {
                MatchFinderKind::Hc5
            }
        } else {
            match num_hash_bytes {
                2 => MatchFinderKind::Bt2,
                3 => MatchFinderKind::Bt3,
                4 => MatchFinderKind::Bt4,
                _ => MatchFinderKind::Bt5,
            }
        }
    }
}

pub(crate) use crate::buffer::Zeroable;

/// `len` zeroes, allocated zeroed rather than allocated and then filled.
///
/// A fresh finder's window, hash and son tables are this. None of them needs
/// a fill of its own: the window is read into before it is read, `init`
/// writes the hash heads every stream, and the son links are written before
/// they are followed. Asking the allocator for zeroed memory lets it hand
/// back pages it knows are zero (fresh pages of a large allocation are)
/// without touching them, so a stream smaller than the dictionary never makes
/// the rest of the tables resident, and a finder built per small stream
/// skips a fill the size of its tables. `resize(len, 0)` wrote every byte.
pub(crate) fn zeroed_vec<T: Zeroable>(len: usize) -> Result<Vec<T>, Error> {
    let v = crate::buffer::zeroed_vec(len)?;
    // Before any page of it is touched, so the faults that fill it come in
    // huge pages.
    crate::enc::huge_pages::advise_vec(&v);
    Ok(v)
}

/// C: `BigAlloc` aligns large tables, windows and handoff buffers before
/// huge-page advice. Keep Vec's original owner and
/// zero allocation: an aligned interior avoids eager initialization by an
/// over-aligned allocator, and never changes the deallocation layout.
#[derive(Default, Debug)]
pub(crate) struct MatchBuffer<T> {
    storage: Vec<T>,
    start: usize,
    len: usize,
}

pub(crate) type MatchTables = MatchBuffer<u32>;
pub(crate) type MatchWindow = MatchBuffer<u8>;

impl<T: Zeroable> MatchBuffer<T> {
    const PAGE_WORDS: usize = (2 << 20) / core::mem::size_of::<T>();

    pub(crate) fn padded_words(len: usize) -> Result<usize, Error> {
        if cfg!(all(target_os = "linux", feature = "std")) && len > Self::PAGE_WORDS / 2 {
            len.checked_add(Self::PAGE_WORDS - 1).ok_or(Error::Alloc)
        } else {
            Ok(len)
        }
    }

    pub(crate) fn new(len: usize) -> Result<Self, Error> {
        Self::new_with(len, crate::buffer::zeroed_vec::<T>)
    }

    fn new_with(
        len: usize,
        mut allocate: impl FnMut(usize) -> Result<Vec<T>, Error>,
    ) -> Result<Self, Error> {
        let mut words = Self::padded_words(len)?;
        let storage = match allocate(words) {
            Ok(storage) => storage,
            // C: BigAlloc falls back to ordinary alignment on refusal. The
            // optional padding must not turn a usable allocation into OOM.
            Err(_) if words != len => {
                words = len;
                allocate(words)?
            }
            Err(error) => return Err(error),
        };
        let start = if words != len {
            let address_words = storage.as_ptr().addr() / core::mem::size_of::<T>();
            address_words.wrapping_neg() & (Self::PAGE_WORDS - 1)
        } else {
            0
        };
        // The padding covers every alignment offset. Advise only the logical
        // table before writes: padding must not add resident huge pages.
        assert!(start <= words - len);
        let tables = Self {
            storage,
            start,
            len,
        };
        crate::enc::huge_pages::advise_slice(&tables);
        Ok(tables)
    }

    pub(crate) fn allocated_words(&self) -> usize {
        self.storage.len()
    }
}

impl MatchWindow {
    #[cfg(feature = "std")]
    pub(crate) fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }

    /// Restore a worker's input extent after a short read, without replacing
    /// the owner or exposing its alignment padding as input.
    #[cfg(feature = "std")]
    pub(crate) fn resize_zeroed(&mut self, len: usize) -> Result<(), Error> {
        if len > self.storage.len() - self.start {
            *self = Self::new(len)?;
        } else {
            if len > self.len {
                self.storage[self.start + self.len..self.start + len].fill(0);
            }
            self.len = len;
        }
        Ok(())
    }
}

impl<T> core::ops::Deref for MatchBuffer<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        &self.storage[self.start..self.start + self.len]
    }
}

impl<T> core::ops::DerefMut for MatchBuffer<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.storage[self.start..self.start + self.len]
    }
}

#[cfg(test)]
impl<T: PartialEq> PartialEq for MatchBuffer<T> {
    fn eq(&self, other: &Self) -> bool {
        self[..] == other[..]
    }
}

/// How many chain heads the hash-chain finder takes ahead: 16 KiB, which
/// stays in the level-one data cache while the coder reads it back.
const HC_HEADS: usize = 1 << 12;

/// C: `CMatchFinder`.
pub(crate) struct MatchFinder {
    /// C: `p->buffer`, as an offset into `buf_base`.
    pub(crate) buffer: usize,
    pub(crate) pos: u32,
    pub(crate) pos_limit: u32,
    /// C: `p->streamPos`. "wrap over Zero is allowed (streamPos < pos)".
    pub(crate) stream_pos: u32,
    pub(crate) len_limit: u32,

    pub(crate) cyclic_buffer_pos: u32,
    /// C: `p->cyclicBufferSize`; "it must be = (historySize + 1)".
    pub(crate) cyclic_buffer_size: u32,

    pub(crate) stream_end_was_reached: bool,
    pub(crate) kind: MatchFinderKind,
    pub(crate) big_hash: bool,

    pub(crate) match_max_len: u32,
    /// C: `p->hash` and `p->son`, one allocation; `son` starts at `son_base`.
    pub(crate) hash: MatchTables,
    pub(crate) son_base: usize,
    pub(crate) hash_mask: u32,
    pub(crate) cut_value: u32,

    pub(crate) buf_base: MatchWindow,
    /// C: `p->directInput`: the input is a slice the threaded finder reads in
    /// place, so [`MatchFinder::create`] allocates no window. Only
    /// [`crate::enc::lz_find_mt::MatchFinderMt`] sets it; this finder always
    /// reads through its window.
    pub(crate) direct_input: bool,

    pub(crate) block_size: u32,
    pub(crate) keep_size_before: u32,
    pub(crate) keep_size_after: u32,

    pub(crate) num_hash_bytes: u32,
    pub(crate) history_size: u32,
    pub(crate) fixed_hash_size: u32,
    pub(crate) num_hash_bytes_min: u8,
    pub(crate) num_hash_out_bits: u8,
    pub(crate) result: Result<(), Error>,
    pub(crate) crc: [u32; 256],
    pub(crate) num_refs: usize,

    pub(crate) expected_data_size: u64,

    /// The hash-chain finders' heads, taken a run of positions ahead:
    /// `heads[at..end]` are the chain heads of `pos` onwards, and the big
    /// hash already holds those positions. Empty for a binary tree.
    heads: Vec<u32>,
    heads_at: usize,
    heads_end: usize,
    /// Whether the heads are taken ahead (see [`MatchFinder::take_head`]).
    /// Only `kernel-ab` and the tests that compare the two turn it off.
    pub(crate) hc_heads: bool,

    /// The threaded finder's hand-off buffers while this finder is the one
    /// in use, so that a later switch back finds them.
    ///
    /// C: `CMatchFinderMt::hashBuf`, which lives in the `CLzmaEnc` whatever
    /// finder `LzmaEnc_Alloc` picks (`C/LzmaEnc.c:435`) and is allocated
    /// once (`C/LzFindMt.c:863`).
    #[cfg(feature = "std")]
    pub(crate) mt_bufs: MatchTables,

    /// How many buffers this finder has allocated since it was made: the
    /// window, the tables, and for the threaded finder its two hand-off
    /// buffers, which count as one.
    #[cfg(test)]
    pub(crate) allocs: u32,
}

/// The byte table the hash functions index.
///
/// C: `MatchFinder_Construct` builds this from `kCrcPoly` (0xEDB88320) with
/// the usual eight-shift loop. It is the standard reflected CRC-32 table — the
/// same one `crate::crc`'s CRC-32 is defined over — so it is derived from that
/// rather than written out again here.
///
/// The identity: a one-shot CRC-32 over the single byte `b` is
/// `table[0xFF ^ b] ^ 0xFF00_0000`, because the init and final XOR are both
/// `0xFFFF_FFFF` and one step of the byte-at-a-time loop is
/// `table[(crc ^ b) & 0xFF] ^ (crc >> 8)`. Inverting it gives the entry.
/// `the_table_is_the_reflected_crc32_table` checks it against the C's loop.
///
/// This is only the table. The hash *functions* over it — `HASH2_CALC` and
/// friends, with their `kLzHash_CrcShift_*` constants — are the port's own and
/// are not a CRC of anything.
fn crc_table() -> [u32; 256] {
    let mut crc = [0u32; 256];
    for (i, slot) in crc.iter_mut().enumerate() {
        *slot = crate::crc::crc32(&[(i as u8) ^ 0xFF]) ^ 0xFF00_0000;
    }
    crc
}

/// C: `cyclicBufferPos - delta + (delta > cyclicBufferPos ? cyclicBufferSize : 0)`,
/// the wrap back around the cyclic buffer that `GetMatchesSpec1`,
/// `SkipMatchesSpec` and `Hc_GetMatchesSpec` all open with.
///
/// The C evaluates it in `UInt32`, where the subtraction is allowed to wrap
/// and the addition brings it back. Rust's `usize` subtraction is not, so the
/// two cases are written out. `delta` is always below `cyclicBufferSize` here
/// — `cmCheck` is what guarantees it — so the result is always in the buffer.
#[inline]
fn cyclic_back(cyclic_buffer_pos: usize, delta: usize, cyclic_buffer_size: u32) -> usize {
    if cyclic_buffer_pos < delta {
        cyclic_buffer_pos + cyclic_buffer_size as usize - delta
    } else {
        cyclic_buffer_pos - delta
    }
}

/// The sizes `MatchFinder_Create` works out before it allocates anything.
///
/// C: the first two thirds of `MatchFinder_Create`, split out so that the
/// encoder can cost a configuration without paying for it — the memory
/// accounting behind `crate::enc::lzma2_enc::Lzma2Encoder::set_mem_limit`.
pub(crate) struct Plan {
    pub(crate) block_size: u32,
    pub(crate) hash_size_sum: usize,
    pub(crate) num_refs: usize,
}

impl MatchFinder {
    /// C: `MatchFinder_Construct` plus `MatchFinder_SetDefaultSettings`.
    pub(crate) fn new() -> Self {
        let crc = crc_table();
        MatchFinder {
            buffer: 0,
            pos: 0,
            pos_limit: 0,
            stream_pos: 0,
            len_limit: 0,
            cyclic_buffer_pos: 0,
            cyclic_buffer_size: 0,
            stream_end_was_reached: false,
            kind: MatchFinderKind::Bt4,
            big_hash: false,
            match_max_len: 0,
            hash: MatchTables::default(),
            son_base: 0,
            hash_mask: 0,
            // C: MatchFinder_SetDefaultSettings.
            cut_value: 32,
            buf_base: MatchWindow::default(),
            direct_input: false,
            block_size: 0,
            keep_size_before: 0,
            keep_size_after: 0,
            num_hash_bytes: 4,
            history_size: 0,
            fixed_hash_size: 0,
            num_hash_bytes_min: 2,
            num_hash_out_bits: 0,
            result: Ok(()),
            crc,
            num_refs: 0,
            expected_data_size: u64::MAX,
            heads: Vec::new(),
            heads_at: 0,
            heads_end: 0,
            hc_heads: true,
            #[cfg(feature = "std")]
            mt_bufs: MatchTables::default(),
            #[cfg(test)]
            allocs: 0,
        }
    }

    /// C: `Inline_MatchFinder_GetNumAvailableBytes` / `GET_AVAIL_BYTES`.
    #[inline]
    pub(crate) fn get_num_available_bytes(&self) -> u32 {
        self.stream_pos.wrapping_sub(self.pos)
    }

    /// C: `MatchFinder_GetPointerToCurrentPos`, as an offset into
    /// [`Self::buf_base`].
    #[inline]
    pub(crate) fn cur(&self) -> usize {
        self.buffer
    }

    /// C: `MatchFinder_ReadBlock`.
    fn read_block(&mut self, stream: &mut dyn SeqInStream) {
        if self.stream_end_was_reached || self.result.is_err() {
            return;
        }
        loop {
            let dest = self.buffer + self.get_num_available_bytes() as usize;
            let size = self.block_size as usize - dest;
            if size == 0 {
                // C: "we call ReadBlock() after NeedMove() and MoveBlock().
                // So we don't execute this branch in normal code flow."
                return;
            }
            match stream.read(&mut self.buf_base[dest..dest + size]) {
                Err(e) => {
                    self.result = Err(e);
                    return;
                }
                Ok(0) => {
                    self.stream_end_was_reached = true;
                    return;
                }
                Ok(n) => {
                    self.stream_pos = self.stream_pos.wrapping_add(n as u32);
                    if self.get_num_available_bytes() > self.keep_size_after {
                        return;
                    }
                }
            }
        }
    }

    /// C: `MatchFinder_MoveBlock`.
    fn move_block(&mut self) {
        if self.buffer < self.keep_size_before as usize {
            // Only a window `plan` cut to a promised stream length is short
            // enough to get here, and only if the stream then supplied more
            // than was promised. That is a bug in the caller of `create`;
            // make it an error instead of an index out of the window.
            self.result = Err(Error::InternalFailure);
            return;
        }
        let offset = self.buffer - self.keep_size_before as usize;
        let keep_before = (offset & (K_BLOCK_MOVE_ALIGN - 1)) + self.keep_size_before as usize;
        self.buffer = keep_before;
        let from = offset & !(K_BLOCK_MOVE_ALIGN - 1);
        let len = keep_before + self.get_num_available_bytes() as usize;
        self.buf_base.copy_within(from..from + len, 0);
    }

    /// C: `MatchFinder_NeedMove`.
    fn need_move(&self) -> bool {
        if self.stream_end_was_reached || self.result.is_err() {
            return false;
        }
        (self.block_size as usize - self.buffer) <= self.keep_size_after as usize
    }

    /// C: `GetBlockSize`.
    fn get_block_size(&self, history_size: u32) -> u32 {
        let mut block_size = self.keep_size_before.wrapping_add(self.keep_size_after);
        if self.keep_size_before < history_size || block_size < self.keep_size_before {
            return 0; // 32-bit overflow
        }
        let k_block_size_max = 0u32.wrapping_sub(K_BLOCK_SIZE_ALIGN);
        let rem = k_block_size_max - block_size;
        let reserve = (block_size >> (if block_size < (1 << 30) { 1 } else { 2 }))
            + (1 << 12)
            + K_BLOCK_MOVE_ALIGN as u32
            + K_BLOCK_SIZE_ALIGN;
        if block_size >= k_block_size_max || rem < K_BLOCK_SIZE_RESERVE_MIN {
            // C: "we reject settings that will be slow"
            return 0;
        }
        if reserve >= rem {
            block_size = k_block_size_max;
        } else {
            block_size += reserve;
            block_size &= !(K_BLOCK_SIZE_ALIGN - 1);
        }
        block_size
    }

    /// C: `MatchFinder_GetHashMask2`. Input is `historySize`.
    fn get_hash_mask2(&self, mut hs: u32) -> u32 {
        if self.num_hash_bytes == 2 {
            return (1 << 16) - 1;
        }
        // C: `if (hs) hs--;`. Not a saturating subtraction in intent; the
        // guard is the C's, and `hs == 0` must stay 0.
        #[allow(clippy::implicit_saturating_sub)]
        if hs != 0 {
            hs -= 1;
        }
        hs |= hs >> 1;
        hs |= hs >> 2;
        hs |= hs >> 4;
        hs |= hs >> 8;
        // C: "we propagated 16 bits in (hs). Low 16 bits must be set later"
        if hs >= (1 << 24) && self.num_hash_bytes == 3 {
            hs = (1 << 24) - 1;
        }
        hs |= (1 << 16) - 1; // C: "don't change it!"
        if self.num_hash_bytes >= 5 {
            hs |= (256 << K_LZ_HASH_CRC_SHIFT_2) - 1;
        }
        hs
    }

    /// C: `MatchFinder_GetHashMask`. Input is `historySize`.
    fn get_hash_mask(&self, mut hs: u32) -> u32 {
        if self.num_hash_bytes == 2 {
            return (1 << 16) - 1;
        }
        // C: `if (hs) hs--;`. Not a saturating subtraction in intent; the
        // guard is the C's, and `hs == 0` must stay 0.
        #[allow(clippy::implicit_saturating_sub)]
        if hs != 0 {
            hs -= 1;
        }
        hs |= hs >> 1;
        hs |= hs >> 2;
        hs |= hs >> 4;
        hs |= hs >> 8;
        hs >>= 1;
        if hs >= (1 << 24) {
            if self.num_hash_bytes == 3 {
                hs = (1 << 24) - 1;
            } else {
                hs >>= 1;
            }
        }
        hs |= (1 << 16) - 1; // C: "don't change it!"
        if self.num_hash_bytes >= 5 {
            hs |= (256 << K_LZ_HASH_CRC_SHIFT_2) - 1;
        }
        hs
    }

    /// C: `MatchFinder_Create` up to but not including its allocations. Every
    /// field it sets is a scalar the next `create` recomputes.
    pub(crate) fn plan(
        &mut self,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        mut keep_add_buffer_after: u32,
        data_limit: u64,
    ) -> Result<Plan, Error> {
        // C: "we need one additional byte in (p->keepSizeBefore), since we use
        // MoveBlock() after (p->pos++) and before dictionary using"
        self.keep_size_before = history_size
            .checked_add(keep_add_buffer_before)
            .and_then(|v| v.checked_add(1))
            .ok_or(Error::Param)?;

        keep_add_buffer_after += match_max_len;
        // C: "we need (p->keepSizeAfter >= p->numHashBytes)"
        if keep_add_buffer_after < self.num_hash_bytes {
            keep_add_buffer_after = self.num_hash_bytes;
        }
        self.keep_size_after = keep_add_buffer_after;

        let mut block_size = self.get_block_size(history_size);
        if block_size == 0 {
            return Err(Error::Param);
        }
        // A stream that supplies at most `data_limit` bytes fits whole in a
        // window of that many bytes, the look-ahead `ReadBlock` keeps beyond
        // them and one byte more, so that the read which reports the end
        // still has somewhere to go. Such a window is never moved: `NeedMove`
        // asks for less than `keepSizeAfter` free bytes past `buffer`, and
        // `buffer` cannot pass the last byte supplied. What is found in it is
        // what is found in the C's window, which differs only in how much it
        // can hold; see `crate::enc::stream` for why the read sizes do not
        // matter.
        let whole = data_limit
            .saturating_add(u64::from(self.keep_size_after))
            .saturating_add(u64::from(K_BLOCK_SIZE_ALIGN));
        if whole < u64::from(block_size) {
            block_size = (whole as u32) & !(K_BLOCK_SIZE_ALIGN - 1);
        }

        let hs;
        let hs_cur;
        if self.num_hash_out_bits != 0 {
            let mut num_bits = u32::from(self.num_hash_out_bits);
            let nb_max = match self.num_hash_bytes {
                2 => 16,
                3 => 24,
                _ => 32,
            };
            if num_bits >= nb_max {
                num_bits = nb_max;
            }
            let mut h = if num_bits >= 32 {
                u32::MAX
            } else {
                (1u32 << num_bits) - 1
            };
            h |= (1 << 16) - 1; // C: "don't change it!"
            if self.num_hash_bytes >= 5 {
                h |= (256 << K_LZ_HASH_CRC_SHIFT_2) - 1;
            }
            let hs2 = self.get_hash_mask2(history_size);
            if h >= hs2 {
                h = hs2;
            }
            hs = h;
            let mut cur = hs;
            if self.expected_data_size < u64::from(history_size) {
                let hs2 = self.get_hash_mask2(self.expected_data_size as u32);
                if cur >= hs2 {
                    cur = hs2;
                }
            }
            hs_cur = cur;
        } else {
            hs = self.get_hash_mask(history_size);
            let mut cur = hs;
            if self.expected_data_size < u64::from(history_size) {
                cur = self.get_hash_mask(self.expected_data_size as u32);
                if cur >= hs {
                    cur = hs; // C: "is it possible?"
                }
            }
            hs_cur = cur;
        }

        self.hash_mask = hs_cur;

        let mut hash_size_sum = hs as usize + 1;
        {
            let mut fixed_hash_size = 0;
            if self.num_hash_bytes > 2 && self.num_hash_bytes_min <= 2 {
                fixed_hash_size += K_HASH2_SIZE;
            }
            if self.num_hash_bytes > 3 && self.num_hash_bytes_min <= 3 {
                fixed_hash_size += K_HASH3_SIZE;
            }
            hash_size_sum += fixed_hash_size as usize;
            self.fixed_hash_size = fixed_hash_size;
        }

        self.match_max_len = match_max_len;

        // C: `newCyclicBufferSize = historySize + 1; // do not change it`
        let new_cyclic_buffer_size = history_size + 1;
        self.history_size = history_size;
        self.cyclic_buffer_size = new_cyclic_buffer_size;

        let mut num_sons = new_cyclic_buffer_size as usize;
        if self.kind.bt_mode() {
            num_sons <<= 1;
        }
        let mut new_size = hash_size_sum + num_sons;
        // C: "aligned size is not required here, but it can be better for some loops"
        new_size = (new_size + NUM_REFS_ALIGN_MASK) & !NUM_REFS_ALIGN_MASK;

        Ok(Plan {
            block_size,
            hash_size_sum,
            num_refs: new_size,
        })
    }

    /// What [`MatchFinder::create`] would allocate for this configuration, in
    /// bytes: the window, unless `window` is false (C: `directInput`, which
    /// creates none), plus the hash and son tables.
    pub(crate) fn mem_usage(
        &mut self,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
        window: bool,
    ) -> Result<u64, Error> {
        let plan = self.plan(
            history_size,
            keep_add_buffer_before,
            match_max_len,
            keep_add_buffer_after,
            u64::MAX,
        )?;
        let window = if window {
            MatchWindow::padded_words(plan.block_size as usize)? as u64
        } else {
            0
        };
        let heads = if self.kind.bt_mode() {
            0
        } else {
            HC_HEADS as u64 * 4
        };
        Ok(window + (MatchTables::padded_words(plan.num_refs)? as u64) * 4 + heads)
    }

    /// What this finder has allocated, in bytes: the window and the tables.
    #[cfg(test)]
    pub(crate) fn allocated(&self) -> u64 {
        #[cfg(feature = "std")]
        let mt_bufs = self.mt_bufs.allocated_words();
        #[cfg(not(feature = "std"))]
        let mt_bufs = 0;
        self.buf_base.allocated_words() as u64
            + (self.hash.allocated_words() + self.heads.len() + mt_bufs) as u64 * 4
    }

    /// C: `MatchFinder_Create`.
    ///
    /// `data_limit` is a promise, not the C's `expectedDataSize` hint: the
    /// stream this finder is about to be given supplies at most that many
    /// bytes, `u64::MAX` when nobody can say. It only ever shortens the
    /// window; see [`MatchFinder::plan`].
    pub(crate) fn create(
        &mut self,
        history_size: u32,
        keep_add_buffer_before: u32,
        match_max_len: u32,
        keep_add_buffer_after: u32,
        data_limit: u64,
    ) -> Result<(), Error> {
        let plan = self.plan(
            history_size,
            keep_add_buffer_before,
            match_max_len,
            keep_add_buffer_after,
            data_limit,
        )?;

        // C: `LzInWindow_Create2`, which reallocates unless the size is the
        // same. A window that is already long enough is kept here, as the
        // tables below are: an encoder that is given one input after another
        // then allocates for the largest of them once. `block_size` is the
        // window's length from here on, whatever the allocation's.
        // C: with `directInput` there is no window to create. One kept from
        // an earlier stream stays for the next stream that needs it.
        self.block_size = plan.block_size;
        if !self.direct_input && self.buf_base.len() < plan.block_size as usize {
            self.buf_base = MatchWindow::default();
            self.buf_base = MatchWindow::new(plan.block_size as usize)?;
            #[cfg(test)]
            {
                self.allocs += 1;
            }
        }

        self.heads_at = 0;
        self.heads_end = 0;
        #[cfg(feature = "kernel-ab")]
        {
            self.hc_heads = crate::kernel_ab::hc_heads();
        }
        if self.kind.bt_mode() {
            self.heads = Vec::new();
        } else if self.heads.is_empty() {
            self.heads = zeroed_vec(HC_HEADS)?;
        }

        // C 22.02: "we don't reallocate buffer, if old size is enough"
        if !self.hash.is_empty() && self.num_refs >= plan.num_refs {
            self.son_base = plan.hash_size_sum;
            return Ok(());
        }

        self.hash = MatchTables::default();
        self.num_refs = plan.num_refs;
        self.hash = MatchTables::new(plan.num_refs)?;
        self.son_base = plan.hash_size_sum;
        #[cfg(test)]
        {
            self.allocs += 1;
        }
        Ok(())
    }

    /// C: `MatchFinder_SetLimits`.
    fn set_limits(&mut self) {
        let mut n = K_MAX_VAL_FOR_NORMALIZE.wrapping_sub(self.pos);
        if n == 0 {
            // C: "we allow (pos == 0) at start even with (kMaxValForNormalize == 0)"
            n = u32::MAX;
        }

        let k = self.cyclic_buffer_size - self.cyclic_buffer_pos;
        if k < n {
            n = k;
        }

        let mut k = self.get_num_available_bytes();
        {
            let ksa = self.keep_size_after;
            let mut mm = self.match_max_len;
            if k > ksa {
                // C: "we must limit exactly to keepSizeAfter for ReadBlock"
                k -= ksa;
            } else if k >= mm {
                // C: "the limitation for (p->lenLimit) update"
                k -= mm;
                k += 1;
            } else {
                mm = k;
                if k != 0 {
                    k = 1;
                }
            }
            self.len_limit = mm;
        }
        if k < n {
            n = k;
        }

        // C uses UInt32 wrap to reach the normalization position (zero).
        self.pos_limit = self.pos.wrapping_add(n);
    }

    /// C: `MatchFinder_Init_LowHash`.
    fn init_low_hash(&mut self) {
        let n = self.fixed_hash_size as usize;
        self.hash[..n].fill(K_EMPTY_HASH_VALUE);
    }

    /// C: `MatchFinder_Init_HighHash`.
    fn init_high_hash(&mut self) {
        let from = self.fixed_hash_size as usize;
        let n = self.hash_mask as usize + 1;
        self.hash[from..from + n].fill(K_EMPTY_HASH_VALUE);
    }

    /// C: `MatchFinder_Init_4`.
    fn init_4(&mut self) {
        self.buffer = 0;
        // C: "kEmptyHashValue = 0 (Zero) is used in hash tables as NO-VALUE
        // marker" — 1 is "it's smallest optimal value. do not change it".
        self.pos = 1;
        self.stream_pos = 1;
        self.result = Ok(());
        self.stream_end_was_reached = false;
    }

    /// C: `MatchFinder_Init`.
    pub(crate) fn init(&mut self, stream: &mut dyn SeqInStream) {
        self.heads_at = 0;
        self.heads_end = 0;
        self.init_high_hash();
        self.init_low_hash();
        self.init_4();
        if self.direct_input {
            // C: MatchFinder_SET_DIRECT_INPUT_BUF. The MtCoder-owned input
            // is installed before Init and is bounded to one GiB, so it is
            // all visible without moving a window or normalizing positions.
            assert!(self.buf_base.len() <= 1 << 30);
            self.stream_pos += self.buf_base.len() as u32;
            self.stream_end_was_reached = true;
        } else {
            self.read_block(stream);
        }
        // C: `CYC_TO_POS_OFFSET` is 0, so this is `cyclicBufferPos = pos`.
        self.cyclic_buffer_pos = self.pos;
        self.set_limits();
    }

    /// C: `MatchFinder_CheckLimits`. "call only after (p->pos++) update".
    fn check_limits(&mut self, stream: &mut dyn SeqInStream) {
        if self.keep_size_after == self.get_num_available_bytes() {
            // C: "we try to read only in exact state"
            if self.need_move() {
                self.move_block();
            }
            self.read_block(stream);
        }

        if self.pos == K_MAX_VAL_FOR_NORMALIZE
            // C: "optional optimization for last bytes of data"
            && self.get_num_available_bytes() >= self.num_hash_bytes
        {
            // C: "after normalization we need (p->pos >= p->historySize + 1)"
            let sub_value = self.pos.wrapping_sub(self.history_size).wrapping_sub(1);
            // C: `MatchFinder_REDUCE_OFFSETS`.
            self.pos = self.pos.wrapping_sub(sub_value);
            self.stream_pos = self.stream_pos.wrapping_sub(sub_value);
            let hash_items = self.hash_mask as usize + 1 + self.fixed_hash_size as usize;
            normalize3(sub_value, &mut self.hash[..hash_items]);
            let mut num_son_refs = self.cyclic_buffer_size as usize;
            if self.kind.bt_mode() {
                num_son_refs <<= 1;
            }
            let son = self.son_base;
            normalize3(sub_value, &mut self.hash[son..son + num_son_refs]);
        }

        if self.cyclic_buffer_pos == self.cyclic_buffer_size {
            self.cyclic_buffer_pos = 0;
        }

        self.set_limits();
    }

    /// C: the `MOVE_POS` macro.
    #[inline]
    fn move_pos_macro(&mut self, stream: &mut dyn SeqInStream) {
        self.cyclic_buffer_pos += 1;
        self.buffer += 1;
        let pos1 = self.pos.wrapping_add(1);
        self.pos = pos1;
        if pos1 == self.pos_limit {
            self.check_limits(stream);
        }
    }

    /// C: `MatchFinder_MovePos`. "we go here at the end of stream data, when
    /// (avail < num_hash_bytes)".
    fn move_pos(&mut self, stream: &mut dyn SeqInStream) {
        self.move_pos_macro(stream);
    }

    // -----------------------------------------------------------------------
    // Hash calculations. C: the `HASH*_CALC` macros at the top of `LzFind.c`.
    // -----------------------------------------------------------------------

    /// C: `HASH2_CALC`. "if (hv) match, then cur[0] and cur[1] also match".
    #[inline]
    fn hash2_calc(&self, cur: usize) -> u32 {
        u32::from(u16::from_le_bytes([
            self.buf_base[cur],
            self.buf_base[cur + 1],
        ]))
    }

    /// C: `HASH3_CALC`, returning `(h2, hv)`.
    #[inline]
    fn hash3_calc(&self, cur: usize) -> (u32, u32) {
        let b = &self.buf_base[cur..cur + 3];
        let temp = self.crc[usize::from(b[0])] ^ u32::from(b[1]);
        let h2 = temp & (K_HASH2_SIZE - 1);
        let hv = (temp ^ (u32::from(b[2]) << 8)) & self.hash_mask;
        (h2, hv)
    }

    /// C: `HASH4_CALC`, returning `(h2, h3, hv)`.
    #[inline]
    fn hash4_calc(&self, cur: usize) -> (u32, u32, u32) {
        let b = &self.buf_base[cur..cur + 4];
        let mut temp = self.crc[usize::from(b[0])] ^ u32::from(b[1]);
        let h2 = temp & (K_HASH2_SIZE - 1);
        temp ^= u32::from(b[2]) << 8;
        let h3 = temp & (K_HASH3_SIZE - 1);
        let hv = (temp ^ (self.crc[usize::from(b[3])] << K_LZ_HASH_CRC_SHIFT_1)) & self.hash_mask;
        (h2, h3, hv)
    }

    /// C: `HASH5_CALC`, returning `(h2, h3, hv)`.
    #[inline]
    fn hash5_calc(&self, cur: usize) -> (u32, u32, u32) {
        let b = &self.buf_base[cur..cur + 5];
        let mut temp = self.crc[usize::from(b[0])] ^ u32::from(b[1]);
        let h2 = temp & (K_HASH2_SIZE - 1);
        temp ^= u32::from(b[2]) << 8;
        let h3 = temp & (K_HASH3_SIZE - 1);
        temp ^= self.crc[usize::from(b[3])] << K_LZ_HASH_CRC_SHIFT_1;
        let hv = (temp ^ (self.crc[usize::from(b[4])] << K_LZ_HASH_CRC_SHIFT_2)) & self.hash_mask;
        (h2, h3, hv)
    }

    // -----------------------------------------------------------------------
    // GetMatches. C: the `*_MatchFinder_GetMatches` family.
    // -----------------------------------------------------------------------

    /// C: the `GetMatches` slot of `IMatchFinder2`, dispatched as
    /// `MatchFinder_CreateVTable` does.
    ///
    /// Writes `(len, dist - 1)` pairs into `distances` and returns how many
    /// `u32`s were written, which is the C's `d - p->matches`.
    pub(crate) fn get_matches(
        &mut self,
        stream: &mut dyn SeqInStream,
        distances: &mut [u32],
    ) -> usize {
        match self.kind {
            MatchFinderKind::Bt2 => self.bt2_get_matches(stream, distances),
            MatchFinderKind::Bt3 => self.bt3_get_matches(stream, distances),
            MatchFinderKind::Bt4 => self.bt4_get_matches(stream, distances),
            MatchFinderKind::Bt5 => self.bt5_get_matches(stream, distances),
            MatchFinderKind::Hc4 => self.hc4_get_matches(stream, distances),
            MatchFinderKind::Hc5 => self.hc5_get_matches(stream, distances),
        }
    }

    /// C: the `Skip` slot of `IMatchFinder2`.
    pub(crate) fn skip(&mut self, stream: &mut dyn SeqInStream, num: u32) {
        match self.kind {
            MatchFinderKind::Bt2 => self.bt2_skip(stream, num),
            MatchFinderKind::Bt3 => self.bt3_skip(stream, num),
            MatchFinderKind::Bt4 => self.bt4_skip(stream, num),
            MatchFinderKind::Bt5 => self.bt5_skip(stream, num),
            MatchFinderKind::Hc4 => self.hc4_skip(stream, num),
            MatchFinderKind::Hc5 => self.hc5_skip(stream, num),
        }
    }

    /// C: `SET_mmm`.
    #[inline]
    fn set_mmm(&self) -> u32 {
        let mmm = self.cyclic_buffer_size;
        if self.pos < mmm { self.pos } else { mmm }
    }

    /// C: `UPDATE_maxLen`.
    #[inline]
    fn update_max_len(&self, cur: usize, d2: u32, max_len: u32, len_limit: u32) -> u32 {
        let start = cur + max_len as usize;
        let limit = cur + len_limit as usize;
        (match_run(&self.buf_base, d2 as usize, start, limit) - cur) as u32
    }

    /// C: `Bt2_MatchFinder_GetMatches`.
    fn bt2_get_matches(&mut self, stream: &mut dyn SeqInStream, distances: &mut [u32]) -> usize {
        let len_limit = self.len_limit;
        if len_limit < 2 {
            self.move_pos(stream);
            return 0;
        }
        let cur = self.buffer;
        let hv = self.hash2_calc(cur) as usize;
        let cur_match = self.hash[hv];
        self.hash[hv] = self.pos;
        let n = self.get_matches_spec1(len_limit, cur_match, cur, distances, 1);
        self.move_pos_macro(stream);
        n
    }

    /// C: `Bt3_MatchFinder_GetMatches`.
    fn bt3_get_matches(&mut self, stream: &mut dyn SeqInStream, distances: &mut [u32]) -> usize {
        let len_limit = self.len_limit;
        if len_limit < 3 {
            self.move_pos(stream);
            return 0;
        }
        let cur = self.buffer;
        let (h2, hv) = self.hash3_calc(cur);
        let pos = self.pos;
        let d2 = pos - self.hash[h2 as usize];
        let cur_match = self.hash[K_FIX3_HASH_SIZE + hv as usize];
        self.hash[h2 as usize] = pos;
        self.hash[K_FIX3_HASH_SIZE + hv as usize] = pos;
        let mmm = self.set_mmm();
        let mut max_len = 2u32;
        let mut d = 0usize;

        if d2 < mmm && self.buf_base[cur - d2 as usize] == self.buf_base[cur] {
            max_len = self.update_max_len(cur, d2, max_len, len_limit);
            distances[0] = max_len;
            distances[1] = d2 - 1;
            d = 2;
            if max_len == len_limit {
                self.skip_matches_spec(len_limit, cur_match, cur);
                self.move_pos_macro(stream);
                return d;
            }
        }

        let n = d + self.get_matches_spec1(len_limit, cur_match, cur, &mut distances[d..], max_len);
        self.move_pos_macro(stream);
        n
    }

    /// C: `Bt4_MatchFinder_GetMatches`.
    fn bt4_get_matches(&mut self, stream: &mut dyn SeqInStream, distances: &mut [u32]) -> usize {
        let len_limit = self.len_limit;
        if len_limit < 4 {
            self.move_pos(stream);
            return 0;
        }
        let cur = self.buffer;
        let (h2, h3, hv) = self.hash4_calc(cur);
        let pos = self.pos;
        let mut d2 = pos - self.hash[h2 as usize];
        let d3 = pos - self.hash[K_FIX3_HASH_SIZE + h3 as usize];
        let cur_match = self.hash[K_FIX4_HASH_SIZE + hv as usize];
        self.hash[h2 as usize] = pos;
        self.hash[K_FIX3_HASH_SIZE + h3 as usize] = pos;
        self.hash[K_FIX4_HASH_SIZE + hv as usize] = pos;
        let mmm = self.set_mmm();
        let mut max_len = 3u32;
        let mut d = 0usize;
        let buf_cur = self.buf_base[cur];

        // The C reaches its common tail by falling out of a chain of nested
        // `if`s; `loop { ... break }` is how that chain is written here, so it
        // is deliberately a single pass.
        #[allow(clippy::never_loop)]
        loop {
            if d2 < mmm && self.buf_base[cur - d2 as usize] == buf_cur {
                distances[0] = 2;
                distances[1] = d2 - 1;
                d = 2;
                if self.buf_base[cur - d2 as usize + 2] == self.buf_base[cur + 2] {
                    // C: `// distances[-2] = 3;`
                } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                    d2 = d3;
                    distances[d + 1] = d3 - 1;
                    d += 2;
                } else {
                    break;
                }
            } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                d2 = d3;
                distances[d + 1] = d3 - 1;
                d += 2;
            } else {
                break;
            }

            max_len = self.update_max_len(cur, d2, max_len, len_limit);
            distances[d - 2] = max_len;
            if max_len == len_limit {
                self.skip_matches_spec(len_limit, cur_match, cur);
                self.move_pos_macro(stream);
                return d;
            }
            break;
        }

        let n = d + self.get_matches_spec1(len_limit, cur_match, cur, &mut distances[d..], max_len);
        self.move_pos_macro(stream);
        n
    }

    /// C: `Bt5_MatchFinder_GetMatches`.
    fn bt5_get_matches(&mut self, stream: &mut dyn SeqInStream, distances: &mut [u32]) -> usize {
        let len_limit = self.len_limit;
        if len_limit < 5 {
            self.move_pos(stream);
            return 0;
        }
        let cur = self.buffer;
        let (h2, h3, hv) = self.hash5_calc(cur);
        let pos = self.pos;
        let mut d2 = pos - self.hash[h2 as usize];
        let d3 = pos - self.hash[K_FIX3_HASH_SIZE + h3 as usize];
        let cur_match = self.hash[K_FIX5_HASH_SIZE + hv as usize];
        self.hash[h2 as usize] = pos;
        self.hash[K_FIX3_HASH_SIZE + h3 as usize] = pos;
        self.hash[K_FIX5_HASH_SIZE + hv as usize] = pos;
        let mmm = self.set_mmm();
        let mut max_len = 4u32;
        let mut d = 0usize;
        let buf_cur = self.buf_base[cur];

        // The C reaches its common tail by falling out of a chain of nested
        // `if`s; `loop { ... break }` is how that chain is written here, so it
        // is deliberately a single pass.
        #[allow(clippy::never_loop)]
        loop {
            if d2 < mmm && self.buf_base[cur - d2 as usize] == buf_cur {
                distances[0] = 2;
                distances[1] = d2 - 1;
                d = 2;
                if self.buf_base[cur - d2 as usize + 2] == self.buf_base[cur + 2] {
                } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                    distances[d + 1] = d3 - 1;
                    d += 2;
                    d2 = d3;
                } else {
                    break;
                }
            } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                distances[d + 1] = d3 - 1;
                d += 2;
                d2 = d3;
            } else {
                break;
            }

            distances[d - 2] = 3;
            if self.buf_base[cur - d2 as usize + 3] != self.buf_base[cur + 3] {
                break;
            }
            max_len = self.update_max_len(cur, d2, max_len, len_limit);
            distances[d - 2] = max_len;
            if max_len == len_limit {
                self.skip_matches_spec(len_limit, cur_match, cur);
                self.move_pos_macro(stream);
                return d;
            }
            break;
        }

        let n = d + self.get_matches_spec1(len_limit, cur_match, cur, &mut distances[d..], max_len);
        self.move_pos_macro(stream);
        n
    }

    /// C: `Hc4_MatchFinder_GetMatches`.
    fn hc4_get_matches(&mut self, stream: &mut dyn SeqInStream, distances: &mut [u32]) -> usize {
        let len_limit = self.len_limit;
        if len_limit < 4 {
            self.move_pos(stream);
            return 0;
        }
        let cur = self.buffer;
        let (h2, h3, hv) = self.hash4_calc(cur);
        let pos = self.pos;
        let mut d2 = pos - self.hash[h2 as usize];
        let d3 = pos - self.hash[K_FIX3_HASH_SIZE + h3 as usize];
        self.hash[h2 as usize] = pos;
        self.hash[K_FIX3_HASH_SIZE + h3 as usize] = pos;
        let mmm = self.set_mmm();
        let mut max_len = 3u32;
        let mut d = 0usize;
        let buf_cur = self.buf_base[cur];

        // The C reaches its common tail by falling out of a chain of nested
        // `if`s; `loop { ... break }` is how that chain is written here, so it
        // is deliberately a single pass.
        #[allow(clippy::never_loop)]
        loop {
            if d2 < mmm && self.buf_base[cur - d2 as usize] == buf_cur {
                distances[0] = 2;
                distances[1] = d2 - 1;
                d = 2;
                if self.buf_base[cur - d2 as usize + 2] == self.buf_base[cur + 2] {
                } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                    d2 = d3;
                    distances[d + 1] = d3 - 1;
                    d += 2;
                } else {
                    break;
                }
            } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                d2 = d3;
                distances[d + 1] = d3 - 1;
                d += 2;
            } else {
                break;
            }

            max_len = self.update_max_len(cur, d2, max_len, len_limit);
            distances[d - 2] = max_len;
            if max_len == len_limit {
                let cur_match = self.take_head(pos, cur, K_FIX4_HASH_SIZE, hv, false);
                let at = self.son_base + self.cyclic_buffer_pos as usize;
                self.hash[at] = cur_match;
                self.move_pos_macro(stream);
                return d;
            }
            break;
        }

        let cur_match = self.take_head(pos, cur, K_FIX4_HASH_SIZE, hv, true);
        let n =
            d + self.hc_get_matches_spec(len_limit, cur_match, cur, &mut distances[d..], max_len);
        self.move_pos_macro(stream);
        n
    }

    /// C: `Hc5_MatchFinder_GetMatches`.
    fn hc5_get_matches(&mut self, stream: &mut dyn SeqInStream, distances: &mut [u32]) -> usize {
        let len_limit = self.len_limit;
        if len_limit < 5 {
            self.move_pos(stream);
            return 0;
        }
        let cur = self.buffer;
        let (h2, h3, hv) = self.hash5_calc(cur);
        let pos = self.pos;
        let mut d2 = pos - self.hash[h2 as usize];
        let d3 = pos - self.hash[K_FIX3_HASH_SIZE + h3 as usize];
        self.hash[h2 as usize] = pos;
        self.hash[K_FIX3_HASH_SIZE + h3 as usize] = pos;
        let mmm = self.set_mmm();
        let mut max_len = 4u32;
        let mut d = 0usize;
        let buf_cur = self.buf_base[cur];

        // The C reaches its common tail by falling out of a chain of nested
        // `if`s; `loop { ... break }` is how that chain is written here, so it
        // is deliberately a single pass.
        #[allow(clippy::never_loop)]
        loop {
            if d2 < mmm && self.buf_base[cur - d2 as usize] == buf_cur {
                distances[0] = 2;
                distances[1] = d2 - 1;
                d = 2;
                if self.buf_base[cur - d2 as usize + 2] == self.buf_base[cur + 2] {
                } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                    distances[d + 1] = d3 - 1;
                    d += 2;
                    d2 = d3;
                } else {
                    break;
                }
            } else if d3 < mmm && self.buf_base[cur - d3 as usize] == buf_cur {
                distances[d + 1] = d3 - 1;
                d += 2;
                d2 = d3;
            } else {
                break;
            }

            distances[d - 2] = 3;
            if self.buf_base[cur - d2 as usize + 3] != self.buf_base[cur + 3] {
                break;
            }
            max_len = self.update_max_len(cur, d2, max_len, len_limit);
            distances[d - 2] = max_len;
            if max_len == len_limit {
                let cur_match = self.take_head(pos, cur, K_FIX5_HASH_SIZE, hv, false);
                let at = self.son_base + self.cyclic_buffer_pos as usize;
                self.hash[at] = cur_match;
                self.move_pos_macro(stream);
                return d;
            }
            break;
        }

        let cur_match = self.take_head(pos, cur, K_FIX5_HASH_SIZE, hv, true);
        let n =
            d + self.hc_get_matches_spec(len_limit, cur_match, cur, &mut distances[d..], max_len);
        self.move_pos_macro(stream);
        n
    }

    // -----------------------------------------------------------------------
    // Skip. C: the `*_MatchFinder_Skip` family.
    // -----------------------------------------------------------------------

    /// C: `Bt2_MatchFinder_Skip`.
    fn bt2_skip(&mut self, stream: &mut dyn SeqInStream, mut num: u32) {
        loop {
            let len_limit = self.len_limit;
            if len_limit < 2 {
                self.move_pos(stream);
            } else {
                let cur = self.buffer;
                let hv = self.hash2_calc(cur) as usize;
                let cur_match = self.hash[hv];
                self.hash[hv] = self.pos;
                self.skip_matches_spec(len_limit, cur_match, cur);
                self.move_pos_macro(stream);
            }
            num -= 1;
            if num == 0 {
                return;
            }
        }
    }

    /// C: `Bt3_MatchFinder_Skip`.
    fn bt3_skip(&mut self, stream: &mut dyn SeqInStream, mut num: u32) {
        loop {
            let len_limit = self.len_limit;
            if len_limit < 3 {
                self.move_pos(stream);
            } else {
                let cur = self.buffer;
                let (h2, hv) = self.hash3_calc(cur);
                let cur_match = self.hash[K_FIX3_HASH_SIZE + hv as usize];
                self.hash[h2 as usize] = self.pos;
                self.hash[K_FIX3_HASH_SIZE + hv as usize] = self.pos;
                self.skip_matches_spec(len_limit, cur_match, cur);
                self.move_pos_macro(stream);
            }
            num -= 1;
            if num == 0 {
                return;
            }
        }
    }

    /// C: `Bt4_MatchFinder_Skip`.
    fn bt4_skip(&mut self, stream: &mut dyn SeqInStream, mut num: u32) {
        loop {
            let len_limit = self.len_limit;
            if len_limit < 4 {
                self.move_pos(stream);
            } else {
                let cur = self.buffer;
                let (h2, h3, hv) = self.hash4_calc(cur);
                let cur_match = self.hash[K_FIX4_HASH_SIZE + hv as usize];
                self.hash[h2 as usize] = self.pos;
                self.hash[K_FIX3_HASH_SIZE + h3 as usize] = self.pos;
                self.hash[K_FIX4_HASH_SIZE + hv as usize] = self.pos;
                self.skip_matches_spec(len_limit, cur_match, cur);
                self.move_pos_macro(stream);
            }
            num -= 1;
            if num == 0 {
                return;
            }
        }
    }

    /// C: `Bt5_MatchFinder_Skip`.
    fn bt5_skip(&mut self, stream: &mut dyn SeqInStream, mut num: u32) {
        loop {
            let len_limit = self.len_limit;
            if len_limit < 5 {
                self.move_pos(stream);
            } else {
                let cur = self.buffer;
                let (h2, h3, hv) = self.hash5_calc(cur);
                let cur_match = self.hash[K_FIX5_HASH_SIZE + hv as usize];
                self.hash[h2 as usize] = self.pos;
                self.hash[K_FIX3_HASH_SIZE + h3 as usize] = self.pos;
                self.hash[K_FIX5_HASH_SIZE + hv as usize] = self.pos;
                self.skip_matches_spec(len_limit, cur_match, cur);
                self.move_pos_macro(stream);
            }
            num -= 1;
            if num == 0 {
                return;
            }
        }
    }

    /// C: `Hc4_MatchFinder_Skip`, whose `HC_SKIP_HEADER`/`HC_SKIP_FOOTER`
    /// batch a run of positions between two `posLimit` checks.
    fn hc4_skip(&mut self, stream: &mut dyn SeqInStream, num: u32) {
        self.hc_skip::<4>(stream, num);
    }

    /// C: `Hc5_MatchFinder_Skip`.
    fn hc5_skip(&mut self, stream: &mut dyn SeqInStream, num: u32) {
        self.hc_skip::<5>(stream, num);
    }

    /// C: `HC_SKIP_HEADER` / `HC_SKIP_FOOTER` with the 4- or 5-byte hash.
    fn hc_skip<const HASH_BYTES: usize>(&mut self, stream: &mut dyn SeqInStream, mut num: u32) {
        while num != 0 {
            if self.len_limit < HASH_BYTES as u32 {
                self.move_pos(stream);
                num -= 1;
                continue;
            }
            let mut pos = self.pos;
            let mut num2 = num;
            // C: "(p->pos == p->posLimit) is not allowed here !!!"
            let rem = self.pos_limit.wrapping_sub(pos);
            if num2 >= rem {
                num2 = rem;
            }
            num -= num2;
            let cyc_pos = self.cyclic_buffer_pos;
            self.cyclic_buffer_pos = cyc_pos + num2;
            let cur = self.buffer;
            let count = num2 as usize;
            // C: HC_SKIP_HEADER keeps hash, son and cur in locals until
            // posLimit. Split their disjoint ranges once for that whole run.
            let fix = if HASH_BYTES == 4 {
                K_FIX4_HASH_SIZE
            } else {
                K_FIX5_HASH_SIZE
            };
            let (low, rest) = self.hash.split_at_mut(fix);
            let (high, son) = rest.split_at_mut(self.son_base - fix);
            let (h2, h3) = low.split_at_mut(K_FIX3_HASH_SIZE);
            let son = &mut son[cyc_pos as usize..cyc_pos as usize + count];
            let win = &self.buf_base[cur..cur + count + HASH_BYTES - 1];

            // A preceding search may have already updated high hash heads.
            // Consume those exact heads first; recomputing them would see
            // future positions. Only the low hashes and chain remain to write.
            let queued = count.min(self.heads_end - self.heads_at);
            for b in win[..queued + 2].windows(3) {
                let temp = self.crc[usize::from(b[0])] ^ u32::from(b[1]);
                h2[(temp & (K_HASH2_SIZE - 1)) as usize] = pos;
                h3[((temp ^ (u32::from(b[2]) << 8)) & (K_HASH3_SIZE - 1)) as usize] = pos;
                pos = pos.wrapping_add(1);
            }
            son[..queued].copy_from_slice(&self.heads[self.heads_at..self.heads_at + queued]);
            self.heads_at += queued;

            // C: Hc4_MatchFinder_Skip / Hc5_MatchFinder_Skip. Already-skipped
            // positions need no speculative head queue or second hash pass.
            for (b, link) in win[queued..].windows(HASH_BYTES).zip(&mut son[queued..]) {
                let mut temp = self.crc[usize::from(b[0])] ^ u32::from(b[1]);
                let hash2 = (temp & (K_HASH2_SIZE - 1)) as usize;
                temp ^= u32::from(b[2]) << 8;
                let hash3 = (temp & (K_HASH3_SIZE - 1)) as usize;
                temp ^= self.crc[usize::from(b[3])] << K_LZ_HASH_CRC_SHIFT_1;
                if HASH_BYTES == 5 {
                    temp ^= self.crc[usize::from(b[4])] << K_LZ_HASH_CRC_SHIFT_2;
                }
                let hv = (temp & self.hash_mask) as usize;
                *link = high[hv];
                h2[hash2] = pos;
                h3[hash3] = pos;
                high[hv] = pos;
                pos = pos.wrapping_add(1);
            }

            self.buffer = cur + count;
            self.pos = pos;
            if pos == self.pos_limit {
                self.check_limits(stream);
            }
        }
    }

    // -----------------------------------------------------------------------
    // The tree and chain walks.
    // -----------------------------------------------------------------------

    /// C: `GetMatchesSpec1`. Walks the binary tree at `cur_match`, writing the
    /// improving `(len, dist - 1)` pairs and rebuilding the two sub-trees.
    fn get_matches_spec1(
        &mut self,
        len_limit: u32,
        mut cur_match: u32,
        cur: usize,
        distances: &mut [u32],
        mut max_len: u32,
    ) -> usize {
        let pos = self.pos;
        let cyclic_buffer_pos = self.cyclic_buffer_pos as usize;
        let cyclic_buffer_size = self.cyclic_buffer_size;
        let mut cut_value = self.cut_value;
        let son = self.son_base;

        let mut ptr0 = son + (cyclic_buffer_pos << 1) + 1;
        let mut ptr1 = son + (cyclic_buffer_pos << 1);
        let mut len0 = 0u32;
        let mut len1 = 0u32;
        let mut d = 0usize;

        let mut cm_check = pos.wrapping_sub(cyclic_buffer_size);
        if pos < cyclic_buffer_size {
            cm_check = 0;
        }

        if cm_check < cur_match {
            loop {
                let delta = pos - cur_match;
                let pair =
                    son + (cyclic_back(cyclic_buffer_pos, delta as usize, cyclic_buffer_size) << 1);
                let pb = cur - delta as usize;
                let mut len = if len0 < len1 { len0 } else { len1 } as usize;
                let pair0 = self.hash[pair];
                // C: the byte at `len` decides whether the run is entered at
                // all, and the two loops after it walk to the first difference.
                // `match_run` answers both: it returns `len` unchanged exactly
                // when that first byte differs.
                let run = match_run(
                    &self.buf_base,
                    delta as usize,
                    cur + len,
                    cur + len_limit as usize,
                ) - cur;
                if run != len {
                    len = run;
                    if max_len < len as u32 {
                        max_len = len as u32;
                        distances[d] = len as u32;
                        distances[d + 1] = delta - 1;
                        d += 2;
                        if len == len_limit as usize {
                            self.hash[ptr1] = pair0;
                            self.hash[ptr0] = self.hash[pair + 1];
                            return d;
                        }
                    }
                }
                let buf = &self.buf_base;
                if buf[pb + len] < buf[cur + len] {
                    self.hash[ptr1] = cur_match;
                    cur_match = self.hash[pair + 1];
                    ptr1 = pair + 1;
                    len1 = len as u32;
                } else {
                    self.hash[ptr0] = cur_match;
                    cur_match = self.hash[pair];
                    ptr0 = pair;
                    len0 = len as u32;
                }
                cut_value -= 1;
                if cut_value == 0 || cm_check >= cur_match {
                    break;
                }
            }
        }

        self.hash[ptr0] = K_EMPTY_HASH_VALUE;
        self.hash[ptr1] = K_EMPTY_HASH_VALUE;
        d
    }

    /// C: `SkipMatchesSpec`. `GetMatchesSpec1` without the distances.
    fn skip_matches_spec(&mut self, len_limit: u32, mut cur_match: u32, cur: usize) {
        let pos = self.pos;
        let cyclic_buffer_pos = self.cyclic_buffer_pos as usize;
        let cyclic_buffer_size = self.cyclic_buffer_size;
        let mut cut_value = self.cut_value;
        let son = self.son_base;

        let mut ptr0 = son + (cyclic_buffer_pos << 1) + 1;
        let mut ptr1 = son + (cyclic_buffer_pos << 1);
        let mut len0 = 0u32;
        let mut len1 = 0u32;

        let mut cm_check = pos.wrapping_sub(cyclic_buffer_size);
        if pos < cyclic_buffer_size {
            cm_check = 0;
        }

        if cm_check < cur_match {
            loop {
                let delta = pos - cur_match;
                let pair =
                    son + (cyclic_back(cyclic_buffer_pos, delta as usize, cyclic_buffer_size) << 1);
                let pb = cur - delta as usize;
                let mut len = if len0 < len1 { len0 } else { len1 } as usize;
                let run = match_run(
                    &self.buf_base,
                    delta as usize,
                    cur + len,
                    cur + len_limit as usize,
                ) - cur;
                if run != len {
                    len = run;
                    if len == len_limit as usize {
                        self.hash[ptr1] = self.hash[pair];
                        self.hash[ptr0] = self.hash[pair + 1];
                        return;
                    }
                }
                let buf = &self.buf_base;
                if buf[pb + len] < buf[cur + len] {
                    self.hash[ptr1] = cur_match;
                    cur_match = self.hash[pair + 1];
                    ptr1 = pair + 1;
                    len1 = len as u32;
                } else {
                    self.hash[ptr0] = cur_match;
                    cur_match = self.hash[pair];
                    ptr0 = pair;
                    len0 = len as u32;
                }
                cut_value -= 1;
                if cut_value == 0 || cm_check >= cur_match {
                    break;
                }
            }
        }

        self.hash[ptr0] = K_EMPTY_HASH_VALUE;
        self.hash[ptr1] = K_EMPTY_HASH_VALUE;
    }

    /// The chain head of `pos` (at `cur` in the window, its big-hash value
    /// `hv`), with `pos` entered in the big hash at `fix + hv`: what the C's
    /// hash-chain `GetMatches` and `Skip` read and write there.
    ///
    /// The C does this a position at a time, so each head is a load from a
    /// table of megabytes that the chain walk then depends on. Here the heads
    /// of a run of positions are taken in one loop first, as the threaded
    /// finder's hash stage does (C: `GetHeads_LOOP` in `LzFindMt.c`), so
    /// those loads do not wait on one another or on a chain walk. The run
    /// never passes `pos_limit`: up to there `len_limit` stays what it is, so
    /// every position in the run is one the coder hashes, the bytes the hash
    /// reads are in the window, and the run is used up before
    /// `check_limits` moves the window or normalizes the tables. Taken in
    /// order, the heads and the table are the ones the C's lookups leave.
    /// A full-length match does not walk the chain: consume an existing head
    /// or perform just the SDK's single lookup instead of starting that queue.
    #[inline]
    fn take_head(&mut self, pos: u32, cur: usize, fix: usize, hv: u32, prefetch: bool) -> u32 {
        if !self.hc_heads || (!prefetch && self.heads_at == self.heads_end) {
            let at = fix + hv as usize;
            let head = self.hash[at];
            self.hash[at] = pos;
            return head;
        }
        if self.heads_at == self.heads_end {
            self.fill_heads(pos, cur, fix);
        }
        let head = self.heads[self.heads_at];
        self.heads_at += 1;
        head
    }

    /// Takes the heads of `pos` onwards, as many as fit in `heads` and do
    /// not pass `pos_limit`. See [`MatchFinder::take_head`].
    #[cold]
    #[inline(never)]
    fn fill_heads(&mut self, mut pos: u32, cur: usize, fix: usize) {
        let n = (self.pos_limit.wrapping_sub(pos) as usize).min(self.heads.len());
        // `K_FIX5_HASH_SIZE` is `K_FIX4_HASH_SIZE`, so the kind says which.
        let five = self.kind == MatchFinderKind::Hc5;
        let (crc, mask) = (&self.crc, self.hash_mask);
        let win = &self.buf_base[cur..cur + n + 3 + usize::from(five)];
        let (heads, table) = (&mut self.heads[..n], &mut self.hash[fix..]);
        for (i, head) in heads.iter_mut().enumerate() {
            let b = &win[i..];
            // C: `HASH4_CALC` / `HASH5_CALC`, the big-hash value only.
            let mut temp = crc[usize::from(b[0])] ^ u32::from(b[1]);
            temp ^= u32::from(b[2]) << 8;
            temp ^= crc[usize::from(b[3])] << K_LZ_HASH_CRC_SHIFT_1;
            if five {
                temp ^= crc[usize::from(b[4])] << K_LZ_HASH_CRC_SHIFT_2;
            }
            let at = (temp & mask) as usize;
            *head = table[at];
            table[at] = pos;
            pos = pos.wrapping_add(1);
        }
        self.heads_at = 0;
        self.heads_end = n;
    }

    /// C: `Hc_GetMatchesSpec`. Walks the hash chain; "(lenLimit > maxLen)".
    fn hc_get_matches_spec(
        &mut self,
        len_limit: u32,
        mut cur_match: u32,
        cur: usize,
        distances: &mut [u32],
        mut max_len: u32,
    ) -> usize {
        let pos = self.pos;
        let cyclic_buffer_pos = self.cyclic_buffer_pos as usize;
        let cyclic_buffer_size = self.cyclic_buffer_size;
        let mut cut_value = self.cut_value;
        let son = self.son_base;
        let lim = cur + len_limit as usize;
        let mut d = 0usize;

        self.hash[son + cyclic_buffer_pos] = cur_match;

        loop {
            if cur_match == 0 {
                break;
            }
            let delta = pos - cur_match;
            if delta >= cyclic_buffer_size {
                break;
            }
            cur_match =
                self.hash[son + cyclic_back(cyclic_buffer_pos, delta as usize, cyclic_buffer_size)];
            let diff = delta as usize;
            let buf = &self.buf_base;
            if buf[cur + max_len as usize] == buf[cur + max_len as usize - diff] {
                // C: the chain walk restarts the scan at `cur` every time
                // rather than at `maxLen`, so the run starts there too.
                let c = match_run(buf, diff, cur, lim);
                if c == lim {
                    distances[d] = (lim - cur) as u32;
                    distances[d + 1] = delta - 1;
                    return d + 2;
                }
                let len = (c - cur) as u32;
                if max_len < len {
                    max_len = len;
                    distances[d] = len;
                    distances[d + 1] = delta - 1;
                    d += 2;
                }
            }
            cut_value -= 1;
            if cut_value == 0 {
                break;
            }
        }

        d
    }
}

mod normalize;
pub(crate) use normalize::normalize3;

#[cfg(test)]
mod tests {
    #[cfg(feature = "std")]
    #[test]
    fn aligned_input_restores_its_extent_and_preserves_owner_on_failure() {
        use super::MatchWindow;
        let mut input = MatchWindow::new((2 << 20) + 17).unwrap();
        let len = input.len();
        let pointer = input.as_ptr();
        let allocation = input.allocated_words();
        input.fill(53);
        input.truncate(7);
        input.resize_zeroed(len).unwrap();
        assert_eq!(input.as_ptr(), pointer);
        assert_eq!(input.allocated_words(), allocation);
        assert_eq!(&input[..7], &[53; 7]);
        assert!(input[7..].iter().all(|&byte| byte == 0));
        assert_eq!(input.resize_zeroed(usize::MAX), Err(crate::Error::Alloc));
        assert_eq!(input.as_ptr(), pointer);
        assert_eq!(input.len(), len);
        assert_eq!(&input[..7], &[53; 7]);
        if cfg!(target_os = "linux") {
            assert_eq!(pointer.addr() % (2 << 20), 0);
            assert_eq!(allocation, len + (2 << 20) - 1);
        }
        assert!(input.storage[..input.start].iter().all(|&byte| byte == 0));
        assert!(
            input.storage[input.start + len..]
                .iter()
                .all(|&byte| byte == 0)
        );
    }

    #[test]
    fn aligned_match_tables_preserve_zeroes_ownership_and_accounting() {
        use super::MatchTables;
        for len in [
            0,
            1,
            4097,
            MatchTables::PAGE_WORDS / 2,
            MatchTables::PAGE_WORDS + 17,
        ] {
            let mut tables = MatchTables::new(len).unwrap();
            assert_eq!(tables.len(), len);
            assert_eq!(
                tables.allocated_words(),
                MatchTables::padded_words(len).unwrap()
            );
            assert!(tables.iter().all(|&value| value == 0));
            if tables.allocated_words() != len {
                assert_eq!(tables.as_ptr().addr() % (2 << 20), 0);
                assert!(tables.start < MatchTables::PAGE_WORDS);
            }
            if len != 0 {
                tables[0] = 17;
                tables[len - 1] = 29;
                let pointer = tables.as_ptr();
                let moved = core::mem::take(&mut tables);
                assert!(tables.is_empty());
                assert_eq!(moved.as_ptr(), pointer);
                assert_eq!(moved[len - 1], 29);
                assert!(moved.storage[..moved.start].iter().all(|&v| v == 0));
                assert!(moved.storage[moved.start + len..].iter().all(|&v| v == 0));
            }
        }
        assert!(matches!(
            MatchTables::new(usize::MAX),
            Err(crate::Error::Alloc)
        ));
    }

    #[test]
    fn aligned_match_tables_fall_back_without_hiding_allocation_failure() {
        use super::MatchTables;
        let len = MatchTables::PAGE_WORDS + 17;
        let mut requests = alloc::vec::Vec::new();
        let tables = MatchTables::new_with(len, |words| {
            requests.push(words);
            if words != len {
                Err(crate::Error::Alloc)
            } else {
                super::zeroed_vec(words)
            }
        })
        .unwrap();
        assert_eq!(tables.len(), len);
        assert_eq!(tables.allocated_words(), len);
        assert!(tables.iter().all(|&value| value == 0));
        assert_eq!(requests.last(), Some(&len));
        assert_eq!(
            requests.len(),
            if MatchTables::padded_words(len).unwrap() == len {
                1
            } else {
                2
            }
        );
        assert!(matches!(
            MatchTables::new_with(len, |_| Err(crate::Error::Alloc)),
            Err(crate::Error::Alloc)
        ));
    }

    /// A zeroed allocation of every [`super::Zeroable`] type is the length
    /// asked for, all zero and writable to its last element, at odd lengths
    /// and across a page; an empty one is empty, and one the allocator
    /// cannot describe is refused. CI runs this under Miri too, which checks
    /// the allocation, the initialisation and the free against the layout.
    #[test]
    fn a_zeroed_vec_is_zeroes_of_every_zeroable_type() {
        fn check<T: super::Zeroable + PartialEq + core::fmt::Debug>(one: T, zero: T) {
            for len in [1usize, 3, 7, 61, 4_097, 70_001] {
                let mut v = super::zeroed_vec::<T>(len).unwrap();
                assert_eq!((v.len(), v.capacity()), (len, len));
                assert!(v.iter().all(|&x| x == zero), "{len}");
                v[len - 1] = one;
                v[0] = one;
                assert_eq!((v[0], v[len - 1]), (one, one));
                v.push(one);
                assert_eq!(v.len(), len + 1);
            }
            assert!(super::zeroed_vec::<T>(0).unwrap().is_empty());
            assert!(matches!(
                super::zeroed_vec::<T>(usize::MAX),
                Err(crate::error::Error::Alloc)
            ));
        }
        check::<u8>(0xA5, 0);
        check::<u32>(0xDEAD_BEEF, 0);
    }

    /// The table [`crc_table`] derives is the one `MatchFinder_Construct`
    /// builds with its own loop over `kCrcPoly`.
    #[test]
    fn the_table_is_the_reflected_crc32_table() {
        const K_CRC_POLY: u32 = 0xEDB8_8320;
        let mut want = [0u32; 256];
        for (i, slot) in want.iter_mut().enumerate() {
            let mut r = i as u32;
            for _ in 0..8 {
                r = (r >> 1) ^ (K_CRC_POLY & (0u32.wrapping_sub(r & 1)));
            }
            *slot = r;
        }
        assert_eq!(crc_table(), want);
        // A few entries written out, so a change to both sides at once still
        // fails.
        assert_eq!(want[0], 0x0000_0000);
        assert_eq!(want[1], 0x7707_3096);
        assert_eq!(want[128], 0xEDB8_8320);
        assert_eq!(want[255], 0x2D02_EF8D);
    }

    use super::*;
    use crate::enc::stream::SliceStream;

    /// Drives a finder over `src` and returns, for each position, the
    /// `(len, dist)` pairs `GetMatches` reported. `dist` is the real distance,
    /// one more than the `distances[i + 1]` the C stores.
    fn matches_at(kind: MatchFinderKind, src: &[u8]) -> Vec<Vec<(u32, u32)>> {
        let mut mf = MatchFinder::new();
        mf.kind = kind;
        mf.num_hash_bytes = kind.num_hash_bytes();
        mf.cut_value = 32;
        mf.create(1 << 16, 1 << 11, 32, LZMA_MATCH_LEN_MAX + 1, u64::MAX)
            .unwrap();
        let mut stream = SliceStream::new(src);
        mf.init(&mut stream);

        let mut out = Vec::new();
        let mut buf = [0u32; (LZMA_MATCH_LEN_MAX * 2 + 2) as usize];
        while mf.get_num_available_bytes() != 0 {
            let n = mf.get_matches(&mut stream, &mut buf);
            out.push(
                buf[..n]
                    .chunks_exact(2)
                    .map(|p| (p[0], p[1] + 1))
                    .collect::<Vec<_>>(),
            );
        }
        out
    }

    /// Every reported match must really be one: the `len` bytes at the current
    /// position must equal the `len` bytes `dist` back, and the pairs must come
    /// out in strictly increasing length as `GetMatchesSpec1` promises.
    fn check_self_consistent(kind: MatchFinderKind, src: &[u8]) {
        let per_pos = matches_at(kind, src);
        assert_eq!(per_pos.len(), src.len(), "{kind:?}: one call per byte");
        for (pos, pairs) in per_pos.iter().enumerate() {
            let mut last_len = 0;
            for &(len, dist) in pairs {
                assert!(len > last_len, "{kind:?} at {pos}: lengths not increasing");
                last_len = len;
                assert!(
                    dist as usize <= pos,
                    "{kind:?} at {pos}: distance {dist} runs off the front"
                );
                assert!(
                    pos + len as usize <= src.len(),
                    "{kind:?} at {pos}: match runs off the end"
                );
                for k in 0..len as usize {
                    assert_eq!(
                        src[pos + k],
                        src[pos - dist as usize + k],
                        "{kind:?} at {pos}: pair ({len},{dist}) is not a match"
                    );
                }
            }
        }
    }

    const KINDS: [MatchFinderKind; 6] = [
        MatchFinderKind::Hc4,
        MatchFinderKind::Hc5,
        MatchFinderKind::Bt2,
        MatchFinderKind::Bt3,
        MatchFinderKind::Bt4,
        MatchFinderKind::Bt5,
    ];

    #[test]
    fn every_reported_match_is_a_match() {
        let mut data = Vec::new();
        for i in 0..4096u32 {
            data.extend_from_slice(&(i % 97).to_le_bytes()[..1]);
            if i % 13 == 0 {
                data.extend_from_slice(b"the quick brown fox");
            }
        }
        for kind in KINDS {
            check_self_consistent(kind, &data);
            check_self_consistent(kind, b"");
            check_self_consistent(kind, b"a");
            check_self_consistent(kind, b"abcabcabcabc");
            check_self_consistent(kind, &[0u8; 1000]);
        }
    }

    /// Skipping must leave the same future matches, low hashes and links as
    /// the SDK-style finder visited one byte at a time. Exercise both sides
    /// of the queued-head boundary, cyclic wrap, real window slides, and a
    /// UInt32 normalization without allocating a four-GiB fixture.
    #[test]
    fn skips_preserve_the_bytewise_finder_across_all_boundaries() {
        fn check(kind: MatchFinderKind, data: &[u8], wrap: bool) {
            let mut actual = MatchFinder::new();
            let mut expected = MatchFinder::new();
            let mut input = SliceStream::new(data);
            let mut oracle_input = SliceStream::new(data);
            for (finder, stream) in [
                (&mut actual, &mut input),
                (&mut expected, &mut oracle_input),
            ] {
                finder.kind = kind;
                finder.num_hash_bytes = kind.num_hash_bytes();
                finder.create(1 << 12, 0, 32, 0, u64::MAX).unwrap();
                finder.init(stream);
                if wrap {
                    let available = finder.get_num_available_bytes();
                    finder.pos = u32::MAX - 127;
                    finder.stream_pos = finder.pos.wrapping_add(available);
                    finder.set_limits();
                }
            }
            actual.hc_heads = true;
            expected.hc_heads = false;
            let mut got = [0; 64];
            let mut want = [0; 64];
            let mut step = 0;
            while actual.get_num_available_bytes() != 0 {
                let n = actual.get_matches(&mut input, &mut got);
                let m = expected.get_matches(&mut oracle_input, &mut want);
                assert_eq!(&got[..n], &want[..m], "{kind:?} step {step}");
                // Small skips interleave with searches; larger ones cross
                // a prepared-head run, cyclic wrap and input refill.
                let skip = [0, 1, 3, 31, 127, 4095, 4097, 16385][step % 8]
                    .min(actual.get_num_available_bytes());
                // SDK Skip requires a positive count; the parser guards
                // zero-length moves before calling the finder.
                if skip != 0 {
                    actual.skip(&mut input, skip);
                }
                for _ in 0..skip {
                    expected.get_matches(&mut oracle_input, &mut want);
                }
                assert_eq!(
                    (
                        actual.pos,
                        actual.buffer,
                        actual.cyclic_buffer_pos,
                        actual.pos_limit
                    ),
                    (
                        expected.pos,
                        expected.buffer,
                        expected.cyclic_buffer_pos,
                        expected.pos_limit
                    )
                );
                assert_eq!(
                    actual.get_num_available_bytes(),
                    expected.get_num_available_bytes()
                );
                let fix = actual.fixed_hash_size as usize;
                assert_eq!(actual.hash[..fix], expected.hash[..fix]);
                assert_eq!(
                    actual.hash[actual.son_base..],
                    expected.hash[expected.son_base..]
                );
                if actual.heads_at == actual.heads_end {
                    assert_eq!(actual.hash, expected.hash);
                }
                step += 1;
            }
            assert!(actual.result.is_ok() && expected.result.is_ok());
            assert_eq!(actual.hash, expected.hash);
        }
        let mut seed = 1u32;
        let mixed: Vec<_> = (0..300_019)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                if i % 8192 < 4096 { 0 } else { seed as u8 }
            })
            .collect();
        for kind in KINDS {
            for data in [b"".as_slice(), b"a", b"abc", b"abcde", &[0; 9001], &mixed] {
                check(kind, data, false);
            }
            check(kind, &mixed, true);
        }
    }

    /// Hand-checked positions.
    ///
    /// `MatchFinder_SetLimits` clamps `lenLimit` to what is left of the stream,
    /// and `GET_MATCHES_HEADER(n)` reports nothing at all once `lenLimit` falls
    /// below the finder's hash width — which is why the deeper finders go quiet
    /// before the end of a short input while `Bt2` keeps going.
    #[test]
    fn hand_checked_positions() {
        // "abXabYabcabc": 'ab' recurs at 0, 3, 6 and 9, and 'abc' at 6 and 9.
        let src = b"abXabYabcabc";
        let bt2 = matches_at(MatchFinderKind::Bt2, src);
        assert!(bt2[0].is_empty(), "nothing has been seen yet");
        assert_eq!(bt2[3], vec![(2, 3)], "\"ab\" at 3 repeats \"ab\" at 0");
        assert_eq!(
            bt2[6],
            vec![(2, 3)],
            "only the nearest of an equal-length pair"
        );
        assert_eq!(bt2[9], vec![(3, 3)], "\"abc\" at 9 repeats \"abc\" at 6");
        assert_eq!(bt2[10], vec![(2, 3)]);
        assert!(bt2[11].is_empty(), "one byte left, below the 2-byte hash");

        // Bt4 hashes four bytes, and at 9 only three remain.
        let bt4 = matches_at(MatchFinderKind::Bt4, src);
        assert_eq!(
            bt4[3],
            vec![(2, 3)],
            "the 2- and 3-byte side hashes still fire"
        );
        assert!(bt4[9].is_empty(), "lenLimit 3 is below the 4-byte hash");
    }

    /// The hash width bounds where a finder stops, and a run of one byte is
    /// capped at `matchMaxLen`.
    #[test]
    fn hash_width_and_match_max_len() {
        let short = b"qrst_____qrst";
        assert_eq!(
            matches_at(MatchFinderKind::Bt2, short)[9],
            vec![(4, 9)],
            "\"qrst\" at 9 repeats \"qrst\" at 0"
        );
        assert_eq!(matches_at(MatchFinderKind::Bt4, short)[9], vec![(4, 9)]);
        assert!(
            matches_at(MatchFinderKind::Bt5, short)[9].is_empty(),
            "four bytes left is below the 5-byte hash"
        );

        let long = b"qrst_____qrst_____________________________________";
        let bt4 = matches_at(MatchFinderKind::Bt4, long);
        // At 5 the underscore run has just begun: four more follow the one at 4.
        assert_eq!(bt4[5], vec![(4, 1)]);
        // At 13 the run ahead can be reached at four distances, each one byte
        // longer, which is exactly the increasing sequence GetMatchesSpec1
        // promises.
        assert_eq!(bt4[13], vec![(2, 6), (3, 7), (4, 8), (5, 9)]);
        // Inside the long run the finder stops at matchMaxLen, which this test
        // built the finder with.
        assert_eq!(bt4[14], vec![(32, 1)]);
        assert_eq!(bt4[19], vec![(31, 1)], "the run is running out");
        assert!(
            bt4[47].is_empty(),
            "three bytes left is below the 4-byte hash"
        );
    }
}
