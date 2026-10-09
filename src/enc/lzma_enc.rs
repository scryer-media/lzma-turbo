//! The LZMA encoder.
//!
//! C: `CLzmaEnc` in `C/LzmaEnc.c` — the range coder driver
//! (`LzmaEnc_CodeOneBlock`), the optimal parser (`GetOptimum`, `Backward`),
//! the fast parser (`GetOptimumFast`), and the price tables that feed them
//! (`FillDistancesPrices`, `FillAlignPrices`).
//!
//! Not ported: the `Z7_ST`-guarded multi-threaded match finder, and
//! `LzmaEnc_Encode`'s progress callback.

use alloc::{vec, vec::Vec};

use crate::enc::consts::*;
use crate::enc::finder::Finder;
use crate::enc::lz_find::MatchFinderKind;
use crate::enc::price::{
    LenEnc, LenPriceEnc, ProbPrices, init_price_tables, lit_enc_get_price,
    lit_enc_matched_get_price, price, price_0, price_1,
};
use crate::enc::props::{self, LzmaEncProps};
use crate::enc::range_enc::RangeEnc;
use crate::enc::stream::{SeqInStream, SeqOutStream};
use crate::error::Error;

/// C: `COptimal`.
#[derive(Clone, Copy)]
struct Optimal {
    price: u32,
    state: u16,
    /// C: `extra`. 0 normal, 1 `LIT : MATCH`, >1 `MATCH (extra-1) : LIT : REP0`.
    extra: u16,
    len: u32,
    dist: u32,
    reps: [u32; LZMA_NUM_REPS],
}

impl Optimal {
    const fn new() -> Self {
        Optimal {
            price: K_INFINITY_PRICE,
            state: 0,
            extra: 0,
            len: 0,
            dist: 0,
            reps: [0; LZMA_NUM_REPS],
        }
    }

    /// C: `MakeAs_Lit`.
    #[inline]
    fn make_as_lit(&mut self) {
        self.dist = MARK_LIT;
        self.extra = 0;
    }

    /// C: `MakeAs_ShortRep`.
    #[inline]
    fn make_as_short_rep(&mut self) {
        self.dist = 0;
        self.extra = 0;
    }

    /// C: `IsShortRep`.
    #[inline]
    const fn is_short_rep(&self) -> bool {
        self.dist == 0
    }
}

/// C: `CSaveState`, the probability model `Lzma2Enc` rolls back when a chunk
/// turns out not to be worth compressing.
///
/// Written by `LzmaEnc_SaveState` and read by `LzmaEnc_RestoreState`, both of
/// which only the LZMA2 encoder calls.
struct SaveState {
    lit_probs: Vec<u16>,
    state: u32,
    reps: [u32; LZMA_NUM_REPS],
    pos_align_encoder: [u16; K_ALIGN_TABLE_SIZE],
    is_rep: [u16; K_NUM_STATES as usize],
    is_rep_g0: [u16; K_NUM_STATES as usize],
    is_rep_g1: [u16; K_NUM_STATES as usize],
    is_rep_g2: [u16; K_NUM_STATES as usize],
    is_match: [[u16; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
    is_rep0_long: [[u16; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
    pos_slot_encoder: [[u16; 1 << K_NUM_POS_SLOT_BITS]; K_NUM_LEN_TO_POS_STATES],
    pos_encoders: [u16; K_NUM_FULL_DISTANCES],
    len_probs: LenEnc,
    rep_len_probs: LenEnc,
}

/// C: `struct CLzmaEnc`.
pub(crate) struct LzmaEnc {
    pub(crate) mf: Finder,

    opt_cur: usize,
    opt_end: usize,

    longest_match_len: u32,
    num_pairs: usize,
    num_avail: u32,

    state: u32,
    num_fast_bytes: u32,
    additional_offset: u32,
    reps: [u32; LZMA_NUM_REPS],
    lp_mask: u32,
    pb_mask: u32,
    lit_probs: Vec<u16>,
    pub(crate) rc: RangeEnc,

    back_res: u32,

    lc: u32,
    lp: u32,
    pb: u32,
    lclp: u32,

    fast_mode: bool,
    /// C: `p->multiThread`.
    multi_thread: bool,
    pub(crate) write_end_mark: bool,
    pub(crate) finished: bool,
    need_init: bool,

    pub(crate) now_pos64: u64,

    match_price_count: u32,
    rep_len_enc_counter: i32,

    dist_table_size: usize,

    pub(crate) dict_size: u32,
    /// The most the stream of the next `prepare` will supply, `u64::MAX` when
    /// that is not known for certain. See [`LzmaEnc::set_data_limit`].
    data_limit: u64,
    result: Result<(), Error>,

    prob_prices: ProbPrices,

    /// C: `p->matches`; "we want {len , dist} pairs to be 8-bytes aligned".
    matches: [u32; (LZMA_MATCH_LEN_MAX * 2 + 2) as usize],

    align_prices: [u32; K_ALIGN_TABLE_SIZE],
    pos_slot_prices: [[u32; K_DIST_TABLE_SIZE_MAX]; K_NUM_LEN_TO_POS_STATES],
    distances_prices: [[u32; K_NUM_FULL_DISTANCES]; K_NUM_LEN_TO_POS_STATES],

    pos_align_encoder: [u16; K_ALIGN_TABLE_SIZE],
    is_rep: [u16; K_NUM_STATES as usize],
    is_rep_g0: [u16; K_NUM_STATES as usize],
    is_rep_g1: [u16; K_NUM_STATES as usize],
    is_rep_g2: [u16; K_NUM_STATES as usize],
    is_match: [[u16; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
    is_rep0_long: [[u16; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
    pos_slot_encoder: [[u16; 1 << K_NUM_POS_SLOT_BITS]; K_NUM_LEN_TO_POS_STATES],
    pos_encoders: [u16; K_NUM_FULL_DISTANCES],

    len_probs: LenEnc,
    rep_len_probs: LenEnc,

    /// C: `p->g_FastPos`, on the heap because it is 16 KiB.
    g_fast_pos: Vec<u8>,

    len_enc: LenPriceEnc,
    rep_len_enc: LenPriceEnc,

    opt: Vec<Optimal>,

    save_state: SaveState,
}

impl LzmaEnc {
    /// C: `LzmaEnc_Construct`.
    pub(crate) fn new() -> Result<Self, Error> {
        let mut p = LzmaEnc {
            mf: Finder::new(),
            opt_cur: 0,
            opt_end: 0,
            longest_match_len: 0,
            num_pairs: 0,
            num_avail: 0,
            state: 0,
            num_fast_bytes: 0,
            additional_offset: 0,
            reps: [0; LZMA_NUM_REPS],
            lp_mask: 0,
            pb_mask: 0,
            lit_probs: Vec::new(),
            rc: RangeEnc::new()?,
            back_res: 0,
            lc: 0,
            lp: 0,
            pb: 0,
            lclp: u32::MAX,
            fast_mode: false,
            multi_thread: false,
            write_end_mark: false,
            finished: false,
            need_init: true,
            now_pos64: 0,
            match_price_count: 0,
            rep_len_enc_counter: 0,
            dist_table_size: 0,
            dict_size: 0,
            data_limit: u64::MAX,
            result: Ok(()),
            prob_prices: init_price_tables(),
            matches: [0; (LZMA_MATCH_LEN_MAX * 2 + 2) as usize],
            align_prices: [0; K_ALIGN_TABLE_SIZE],
            pos_slot_prices: [[0; K_DIST_TABLE_SIZE_MAX]; K_NUM_LEN_TO_POS_STATES],
            distances_prices: [[0; K_NUM_FULL_DISTANCES]; K_NUM_LEN_TO_POS_STATES],
            pos_align_encoder: [0; K_ALIGN_TABLE_SIZE],
            is_rep: [0; K_NUM_STATES as usize],
            is_rep_g0: [0; K_NUM_STATES as usize],
            is_rep_g1: [0; K_NUM_STATES as usize],
            is_rep_g2: [0; K_NUM_STATES as usize],
            is_match: [[0; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
            is_rep0_long: [[0; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
            pos_slot_encoder: [[0; 1 << K_NUM_POS_SLOT_BITS]; K_NUM_LEN_TO_POS_STATES],
            pos_encoders: [0; K_NUM_FULL_DISTANCES],
            len_probs: LenEnc::new(),
            rep_len_probs: LenEnc::new(),
            g_fast_pos: fast_pos_init(),
            len_enc: LenPriceEnc::new(),
            rep_len_enc: LenPriceEnc::new(),
            opt: Vec::new(),
            save_state: SaveState {
                lit_probs: Vec::new(),
                state: 0,
                reps: [0; LZMA_NUM_REPS],
                pos_align_encoder: [0; K_ALIGN_TABLE_SIZE],
                is_rep: [0; K_NUM_STATES as usize],
                is_rep_g0: [0; K_NUM_STATES as usize],
                is_rep_g1: [0; K_NUM_STATES as usize],
                is_rep_g2: [0; K_NUM_STATES as usize],
                is_match: [[0; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
                is_rep0_long: [[0; LZMA_NUM_PB_STATES_MAX]; K_NUM_STATES as usize],
                pos_slot_encoder: [[0; 1 << K_NUM_POS_SLOT_BITS]; K_NUM_LEN_TO_POS_STATES],
                pos_encoders: [0; K_NUM_FULL_DISTANCES],
                len_probs: LenEnc::new(),
                rep_len_probs: LenEnc::new(),
            },
        };
        p.opt
            .try_reserve_exact(K_NUM_OPTS)
            .map_err(|_| Error::Alloc)?;
        p.opt.resize(K_NUM_OPTS, Optimal::new());
        p.set_props(&LzmaEncProps::new())?;
        Ok(p)
    }

    /// C: `LzmaEnc_SetProps`.
    #[allow(clippy::manual_clamp)] // C keeps the two guards apart.
    pub(crate) fn set_props(&mut self, props2: &LzmaEncProps) -> Result<(), Error> {
        let mut props = *props2;
        props.normalize();
        props::check(&props)?;

        if props.dict_size > K_LZMA_MAX_HISTORY_SIZE {
            props.dict_size = K_LZMA_MAX_HISTORY_SIZE;
        }

        self.dict_size = props.dict_size;
        {
            let mut fb = props.fb as u32;
            if fb < 5 {
                fb = 5;
            }
            if fb > LZMA_MATCH_LEN_MAX {
                fb = LZMA_MATCH_LEN_MAX;
            }
            self.num_fast_bytes = fb;
        }
        self.lc = props.lc as u32;
        self.lp = props.lp as u32;
        self.pb = props.pb as u32;
        self.fast_mode = props.algo == 0;

        let bt_mode = props.bt_mode != 0;
        let mut num_hash_bytes = 4;
        if bt_mode {
            if props.num_hash_bytes < 2 {
                num_hash_bytes = 2;
            } else if props.num_hash_bytes < 4 {
                num_hash_bytes = props.num_hash_bytes as u32;
            }
        }
        if props.num_hash_bytes >= 5 {
            num_hash_bytes = 5;
        }
        self.mf.cfg().kind = MatchFinderKind::from_props(bt_mode, num_hash_bytes);
        self.mf.cfg().num_hash_bytes = num_hash_bytes;
        self.mf.cfg().num_hash_out_bits = props.num_hash_out_bits as u8;
        self.mf.cfg().cut_value = props.mc;

        // C: `p->multiThread = (props.numThreads > 1)`.
        self.multi_thread = props.num_threads > 1;

        self.write_end_mark = props.write_end_mark;
        Ok(())
    }

    /// C: `LzmaEnc_SetDataSize`.
    pub(crate) fn set_data_size(&mut self, expected: u64) {
        self.mf.cfg().expected_data_size = expected;
    }

    /// Promises that the stream the next `prepare` is for supplies at most
    /// `limit` bytes, so that the window need be no longer than that.
    ///
    /// This is not `LzmaEnc_SetDataSize`. That one is a hint, which sizes the
    /// hash table and so changes the bytes, and an input longer than the hint
    /// is still encoded correctly. This one changes no byte of the output and
    /// must be true: a caller that cannot be sure - anything read from a
    /// stream of its own - leaves it unset. The promise covers one `prepare`,
    /// which takes it.
    ///
    /// C: none. The C's window is sized from the dictionary and
    /// `LZMA2_KEEP_WINDOW_SIZE` whatever the input, which is 3 MiB for a
    /// 4 KiB dictionary and a 100-byte input.
    pub(crate) fn set_data_limit(&mut self, limit: u64) {
        self.data_limit = limit;
    }

    /// C: `LzmaEnc_WriteProperties`.
    pub(crate) fn write_properties(&self) -> [u8; LZMA_PROPS_SIZE] {
        props::write_properties(self.lc, self.lp, self.pb, self.dict_size)
    }

    // -----------------------------------------------------------------------
    // Position slots. C: the `GetPosSlot*` macros, `LZMA_LOG_BSR` undefined.
    // -----------------------------------------------------------------------

    /// C: `GetPosSlot1(pos)`.
    #[inline]
    fn get_pos_slot1(&self, pos: u32) -> u32 {
        u32::from(self.g_fast_pos[pos as usize])
    }

    /// C: `GetPosSlot2(pos, res)`, i.e. `BSR2_RET`.
    #[inline]
    fn get_pos_slot2(&self, pos: u32) -> u32 {
        let zz = if pos < (1 << (K_NUM_LOG_BITS + 6)) {
            6
        } else {
            6 + K_NUM_LOG_BITS as u32 - 1
        };
        u32::from(self.g_fast_pos[(pos >> zz) as usize]) + zz * 2
    }

    /// C: `GetPosSlot(pos, res)`.
    #[inline]
    fn get_pos_slot(&self, pos: u32) -> u32 {
        if (pos as usize) < K_NUM_FULL_DISTANCES {
            u32::from(self.g_fast_pos[(pos as usize) & (K_NUM_FULL_DISTANCES - 1)])
        } else {
            self.get_pos_slot2(pos)
        }
    }

    // -----------------------------------------------------------------------
    // Prices. C: the `GET_PRICE*` macros over `p->ProbPrices`.
    // -----------------------------------------------------------------------

    /// C: `LIT_PROBS(pos, prevByte)`, as an index into `lit_probs`.
    #[inline]
    fn lit_probs_at(&self, pos: u32, prev_byte: u8) -> usize {
        3 * ((((pos << 8) + u32::from(prev_byte)) & self.lp_mask) << self.lc) as usize
    }

    /// C: `GetPrice_ShortRep`.
    #[inline]
    fn get_price_short_rep(&self, state: u32, pos_state: u32) -> u32 {
        price_0(&self.prob_prices, self.is_rep_g0[state as usize])
            + price_0(
                &self.prob_prices,
                self.is_rep0_long[state as usize][pos_state as usize],
            )
    }

    /// C: `GetPrice_Rep_0`.
    #[inline]
    fn get_price_rep_0(&self, state: u32, pos_state: u32) -> u32 {
        price_1(
            &self.prob_prices,
            self.is_match[state as usize][pos_state as usize],
        ) + price_1(
            &self.prob_prices,
            self.is_rep0_long[state as usize][pos_state as usize],
        ) + price_1(&self.prob_prices, self.is_rep[state as usize])
            + price_0(&self.prob_prices, self.is_rep_g0[state as usize])
    }

    /// C: `GetPrice_PureRep`.
    fn get_price_pure_rep(&self, rep_index: usize, state: u32, pos_state: u32) -> u32 {
        let pp = &self.prob_prices;
        let prob = self.is_rep_g0[state as usize];
        if rep_index == 0 {
            price_0(pp, prob) + price_1(pp, self.is_rep0_long[state as usize][pos_state as usize])
        } else {
            let mut total = price_1(pp, prob);
            let prob = self.is_rep_g1[state as usize];
            if rep_index == 1 {
                total += price_0(pp, prob);
            } else {
                total += price_1(pp, prob);
                total += price(pp, self.is_rep_g2[state as usize], rep_index as u32 - 2);
            }
            total
        }
    }

    /// C: `FillAlignPrices`.
    fn fill_align_prices(&mut self) {
        let pp = &self.prob_prices;
        let probs = &self.pos_align_encoder;
        for i in 0..K_ALIGN_TABLE_SIZE / 2 {
            let mut total = 0u32;
            let mut sym = i as u32;
            let mut m = 1usize;
            let bit = sym & 1;
            sym >>= 1;
            total += price(pp, probs[m], bit);
            m = (m << 1) + bit as usize;
            let bit = sym & 1;
            sym >>= 1;
            total += price(pp, probs[m], bit);
            m = (m << 1) + bit as usize;
            let bit = sym & 1;
            total += price(pp, probs[m], bit);
            m = (m << 1) + bit as usize;
            let prob = probs[m];
            self.align_prices[i] = total + price_0(pp, prob);
            self.align_prices[i + 8] = total + price_1(pp, prob);
        }
    }

    /// C: `FillDistancesPrices`.
    fn fill_distances_prices(&mut self) {
        let mut temp_prices = [0u32; K_NUM_FULL_DISTANCES];
        let pp = self.prob_prices;
        self.match_price_count = 0;

        for i in (K_START_POS_MODEL_INDEX / 2) as usize..K_NUM_FULL_DISTANCES / 2 {
            let pos_slot = self.get_pos_slot1(i as u32);
            let mut footer_bits = (pos_slot >> 1) - 1;
            let mut base = (2 | (pos_slot & 1)) << footer_bits;
            let probs = &self.pos_encoders[(base as usize) * 2..];
            let mut total = 0u32;
            let mut m = 1usize;
            let mut sym = i as u32;
            let offset = 1u32 << footer_bits;
            base += i as u32;

            if footer_bits != 0 {
                loop {
                    let bit = sym & 1;
                    sym >>= 1;
                    total += price(&pp, probs[m], bit);
                    m = (m << 1) + bit as usize;
                    footer_bits -= 1;
                    if footer_bits == 0 {
                        break;
                    }
                }
            }

            let prob = probs[m];
            temp_prices[base as usize] = total + price_0(&pp, prob);
            temp_prices[(base + offset) as usize] = total + price_1(&pp, prob);
        }

        for lps in 0..K_NUM_LEN_TO_POS_STATES {
            let dist_table_size2 = (self.dist_table_size + 1) >> 1;
            let probs = self.pos_slot_encoder[lps];

            for slot in 0..dist_table_size2 {
                let mut sym = slot + (1 << (K_NUM_POS_SLOT_BITS - 1));
                let bit = (sym & 1) as u32;
                sym >>= 1;
                let mut total = price(&pp, probs[sym], bit);
                for _ in 0..4 {
                    let bit = (sym & 1) as u32;
                    sym >>= 1;
                    total += price(&pp, probs[sym], bit);
                }
                let prob = probs[slot + (1 << (K_NUM_POS_SLOT_BITS - 1))];
                self.pos_slot_prices[lps][slot * 2] = total + price_0(&pp, prob);
                self.pos_slot_prices[lps][slot * 2 + 1] = total + price_1(&pp, prob);
            }

            {
                let mut delta = ((K_END_POS_MODEL_INDEX / 2 - 1) - K_NUM_ALIGN_BITS)
                    << K_NUM_BIT_PRICE_SHIFT_BITS;
                for slot in (K_END_POS_MODEL_INDEX / 2) as usize..dist_table_size2 {
                    self.pos_slot_prices[lps][slot * 2] += delta;
                    self.pos_slot_prices[lps][slot * 2 + 1] += delta;
                    delta += 1 << K_NUM_BIT_PRICE_SHIFT_BITS;
                }
            }

            self.distances_prices[lps][0] = self.pos_slot_prices[lps][0];
            self.distances_prices[lps][1] = self.pos_slot_prices[lps][1];
            self.distances_prices[lps][2] = self.pos_slot_prices[lps][2];
            self.distances_prices[lps][3] = self.pos_slot_prices[lps][3];

            let mut i = 4;
            while i < K_NUM_FULL_DISTANCES {
                let slot_price = self.pos_slot_prices[lps][self.get_pos_slot1(i as u32) as usize];
                self.distances_prices[lps][i] = slot_price + temp_prices[i];
                self.distances_prices[lps][i + 1] = slot_price + temp_prices[i + 1];
                i += 2;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Match finding.
    // -----------------------------------------------------------------------

    /// C: the `MOVE_POS(p, num)` macro.
    #[inline]
    fn move_pos(&mut self, stream: &mut dyn SeqInStream, num: u32) {
        self.additional_offset += num;
        self.mf.skip(stream, num);
    }

    /// C: `ReadMatchDistances`, returning `(len, numPairs)`.
    fn read_match_distances(&mut self, stream: &mut dyn SeqInStream) -> (u32, usize) {
        self.additional_offset += 1;
        self.num_avail = self.mf.get_num_available_bytes();
        let num_pairs = self.mf.get_matches(stream, &mut self.matches);

        if num_pairs == 0 {
            return (0, 0);
        }
        let len = self.matches[num_pairs - 2];
        if len != self.num_fast_bytes {
            return (len, num_pairs);
        }
        // C: the tail extension past `numFastBytes`, which the match finder
        // stopped at.
        let mut num_avail = self.num_avail;
        if num_avail > LZMA_MATCH_LEN_MAX {
            num_avail = LZMA_MATCH_LEN_MAX;
        }
        let p1 = self.mf.cur() - 1;
        let mut p2 = p1 + len as usize;
        let dif = self.matches[num_pairs - 1] as usize + 1;
        let lim = p1 + num_avail as usize;
        let buf = self.mf.window();
        while p2 != lim && buf[p2] == buf[p2 - dif] {
            p2 += 1;
        }
        ((p2 - p1) as u32, num_pairs)
    }

    // -----------------------------------------------------------------------
    // The optimal parser.
    // -----------------------------------------------------------------------

    /// C: `Backward`.
    fn backward(&mut self, mut cur: usize) -> u32 {
        let mut wr = cur + 1;
        self.opt_end = wr;

        loop {
            let mut dist = self.opt[cur].dist;
            let mut len = self.opt[cur].len;
            let extra = u32::from(self.opt[cur].extra);
            cur -= len as usize;

            if extra != 0 {
                wr -= 1;
                self.opt[wr].len = len;
                cur -= extra as usize;
                len = extra;
                if extra == 1 {
                    self.opt[wr].dist = dist;
                    dist = MARK_LIT;
                } else {
                    self.opt[wr].dist = 0;
                    len -= 1;
                    wr -= 1;
                    self.opt[wr].dist = MARK_LIT;
                    self.opt[wr].len = 1;
                }
            }

            if cur == 0 {
                self.back_res = dist;
                self.opt_cur = wr;
                return len;
            }

            wr -= 1;
            self.opt[wr].dist = dist;
            self.opt[wr].len = len;
        }
    }

    /// C: `GetOptimum`.
    #[allow(clippy::too_many_lines)]
    fn get_optimum(&mut self, stream: &mut dyn SeqInStream, mut position: u32) -> u32 {
        let mut last;
        let mut cur;
        let mut reps = [0u32; LZMA_NUM_REPS];
        let mut rep_lens = [0usize; LZMA_NUM_REPS];

        {
            self.opt_cur = 0;
            self.opt_end = 0;

            let (main_len, num_pairs) = if self.additional_offset == 0 {
                self.read_match_distances(stream)
            } else {
                (self.longest_match_len, self.num_pairs)
            };
            let mut num_pairs = num_pairs;

            let mut num_avail = self.num_avail;
            if num_avail < 2 {
                self.back_res = MARK_LIT;
                return 1;
            }
            if num_avail > LZMA_MATCH_LEN_MAX {
                num_avail = LZMA_MATCH_LEN_MAX;
            }

            let data = self.mf.cur() - 1;
            let mut rep_max_index = 0usize;

            // C indexes `reps`, `repLens` and `p->reps` together by `i`.
            #[allow(clippy::needless_range_loop)]
            for i in 0..LZMA_NUM_REPS {
                reps[i] = self.reps[i];
                let data2 = data - reps[i] as usize;
                let buf = self.mf.window();
                if buf[data] != buf[data2] || buf[data + 1] != buf[data2 + 1] {
                    rep_lens[i] = 0;
                    continue;
                }
                let mut len = 2usize;
                while (len as u32) < num_avail && buf[data + len] == buf[data2 + len] {
                    len += 1;
                }
                rep_lens[i] = len;
                if len > rep_lens[rep_max_index] {
                    rep_max_index = i;
                }
                if len as u32 == LZMA_MATCH_LEN_MAX {
                    // C 21.03 optimization.
                    break;
                }
            }

            if rep_lens[rep_max_index] as u32 >= self.num_fast_bytes {
                self.back_res = rep_max_index as u32;
                let len = rep_lens[rep_max_index] as u32;
                self.move_pos(stream, len - 1);
                return len;
            }

            if main_len >= self.num_fast_bytes {
                self.back_res = self.matches[num_pairs - 1] + LZMA_NUM_REPS as u32;
                self.move_pos(stream, main_len - 1);
                return main_len;
            }

            let cur_byte = u32::from(self.mf.window()[data]);
            let match_byte = u32::from(self.mf.window()[data - reps[0] as usize]);

            last = rep_lens[rep_max_index];
            if last <= main_len as usize {
                last = main_len as usize;
            }

            if last < 2 && cur_byte != match_byte {
                self.back_res = MARK_LIT;
                return 1;
            }

            self.opt[0].state = self.state as u16;

            let pos_state = position & self.pb_mask;

            {
                let at = self.lit_probs_at(position, self.mf.window()[data - 1]);
                let probs = &self.lit_probs[at..at + 0x300];
                self.opt[1].price = price_0(
                    &self.prob_prices,
                    self.is_match[self.state as usize][pos_state as usize],
                ) + if is_lit_state(self.state) {
                    lit_enc_get_price(probs, cur_byte, &self.prob_prices)
                } else {
                    lit_enc_matched_get_price(probs, cur_byte, match_byte, &self.prob_prices)
                };
            }
            self.opt[1].make_as_lit();

            let match_price = price_1(
                &self.prob_prices,
                self.is_match[self.state as usize][pos_state as usize],
            );
            let rep_match_price =
                match_price + price_1(&self.prob_prices, self.is_rep[self.state as usize]);

            // C 18.06.
            if match_byte == cur_byte && rep_lens[0] == 0 {
                let short_rep_price =
                    rep_match_price + self.get_price_short_rep(self.state, pos_state);
                if short_rep_price < self.opt[1].price {
                    self.opt[1].price = short_rep_price;
                    self.opt[1].make_as_short_rep();
                }
                if last < 2 {
                    self.back_res = self.opt[1].dist;
                    return 1;
                }
            }

            self.opt[1].len = 1;
            self.opt[0].reps = reps;

            // ---------- REP ----------
            #[allow(clippy::needless_range_loop)] // C indexes by `i`.
            for i in 0..LZMA_NUM_REPS {
                let mut rep_len = rep_lens[i];
                if rep_len < 2 {
                    continue;
                }
                let base = rep_match_price + self.get_price_pure_rep(i, self.state, pos_state);
                loop {
                    let price2 = base + self.rep_len_enc.get_price_len(pos_state, rep_len as u32);
                    let opt = &mut self.opt[rep_len];
                    if price2 < opt.price {
                        opt.price = price2;
                        opt.len = rep_len as u32;
                        opt.dist = i as u32;
                        opt.extra = 0;
                    }
                    rep_len -= 1;
                    if rep_len < 2 {
                        break;
                    }
                }
            }

            // ---------- MATCH ----------
            {
                let mut len = rep_lens[0] + 1;
                if len <= main_len as usize {
                    let mut offs = 0usize;
                    let normal_match_price =
                        match_price + price_0(&self.prob_prices, self.is_rep[self.state as usize]);

                    if len < 2 {
                        len = 2;
                    } else {
                        while len as u32 > self.matches[offs] {
                            offs += 2;
                        }
                    }

                    loop {
                        let dist = self.matches[offs + 1];
                        let mut price2 =
                            normal_match_price + self.len_enc.get_price_len(pos_state, len as u32);
                        let len_to_pos_state = get_len_to_pos_state(len as u32);

                        if (dist as usize) < K_NUM_FULL_DISTANCES {
                            price2 += self.distances_prices[len_to_pos_state]
                                [dist as usize & (K_NUM_FULL_DISTANCES - 1)];
                        } else {
                            let slot = self.get_pos_slot2(dist);
                            price2 += self.align_prices[(dist & K_ALIGN_MASK) as usize];
                            price2 += self.pos_slot_prices[len_to_pos_state][slot as usize];
                        }

                        let opt = &mut self.opt[len];
                        if price2 < opt.price {
                            opt.price = price2;
                            opt.len = len as u32;
                            opt.dist = dist + LZMA_NUM_REPS as u32;
                            opt.extra = 0;
                        }

                        if len as u32 == self.matches[offs] {
                            offs += 2;
                            if offs == num_pairs {
                                break;
                            }
                        }
                        len += 1;
                    }
                }
            }

            cur = 0usize;

            // ---------- Optimal Parsing ----------
            loop {
                cur += 1;
                if cur == last {
                    break;
                }

                // C 18.06: the window is nearly full; take the cheapest state
                // ahead and stop.
                if cur >= K_NUM_OPTS - 64 {
                    let mut price = self.opt[cur].price;
                    let mut best = cur;
                    for j in cur + 1..=last {
                        let price2 = self.opt[j].price;
                        if price >= price2 {
                            price = price2;
                            best = j;
                        }
                    }
                    let delta = best - cur;
                    if delta != 0 {
                        self.move_pos(stream, delta as u32);
                    }
                    cur = best;
                    break;
                }

                let (new_len, pairs) = self.read_match_distances(stream);
                let mut new_len = new_len;
                num_pairs = pairs;

                if new_len >= self.num_fast_bytes {
                    self.num_pairs = num_pairs;
                    self.longest_match_len = new_len;
                    break;
                }

                position += 1;

                let mut prev = cur - self.opt[cur].len as usize;
                let state;

                if self.opt[cur].len == 1 {
                    let s = u32::from(self.opt[prev].state);
                    state = if self.opt[cur].is_short_rep() {
                        u32::from(K_SHORT_REP_NEXT_STATES[s as usize])
                    } else {
                        u32::from(K_LITERAL_NEXT_STATES[s as usize])
                    };
                } else {
                    let dist = self.opt[cur].dist;
                    let extra = u32::from(self.opt[cur].extra);

                    if extra != 0 {
                        prev -= extra as usize;
                        state = if extra == 1 {
                            if (dist as usize) < LZMA_NUM_REPS {
                                K_STATE_REP_AFTER_LIT
                            } else {
                                K_STATE_MATCH_AFTER_LIT
                            }
                        } else {
                            K_STATE_REP_AFTER_LIT
                        };
                    } else {
                        let s = u32::from(self.opt[prev].state);
                        state = if (dist as usize) < LZMA_NUM_REPS {
                            u32::from(K_REP_NEXT_STATES[s as usize])
                        } else {
                            u32::from(K_MATCH_NEXT_STATES[s as usize])
                        };
                    }

                    let prev_reps = self.opt[prev].reps;
                    let b0 = prev_reps[0];

                    if (dist as usize) < LZMA_NUM_REPS {
                        if dist == 0 {
                            reps[0] = b0;
                            reps[1] = prev_reps[1];
                            reps[2] = prev_reps[2];
                            reps[3] = prev_reps[3];
                        } else {
                            reps[1] = b0;
                            let b0 = prev_reps[1];
                            if dist == 1 {
                                reps[0] = b0;
                                reps[2] = prev_reps[2];
                                reps[3] = prev_reps[3];
                            } else {
                                reps[2] = b0;
                                reps[0] = prev_reps[dist as usize];
                                reps[3] = prev_reps[(dist ^ 1) as usize];
                            }
                        }
                    } else {
                        reps[0] = dist - LZMA_NUM_REPS as u32 + 1;
                        reps[1] = b0;
                        reps[2] = prev_reps[1];
                        reps[3] = prev_reps[2];
                    }
                }

                self.opt[cur].state = state as u16;
                self.opt[cur].reps = reps;

                let data = self.mf.cur() - 1;
                let cur_byte = u32::from(self.mf.window()[data]);
                let match_byte = u32::from(self.mf.window()[data - reps[0] as usize]);

                let pos_state = position & self.pb_mask;

                // The order of price checks:
                //    <  LIT
                //    <= SHORT_REP
                //    <  LIT : REP_0
                //    <  REP    [ : LIT : REP_0 ]
                //    <  MATCH  [ : LIT : REP_0 ]
                let cur_price = self.opt[cur].price;
                let prob = self.is_match[state as usize][pos_state as usize];
                let match_price = cur_price + price_1(&self.prob_prices, prob);
                let mut lit_price = cur_price + price_0(&self.prob_prices, prob);

                let mut next_is_lit = false;

                // C 18.new.06.
                if (self.opt[cur + 1].price < K_INFINITY_PRICE && match_byte == cur_byte)
                    || lit_price > self.opt[cur + 1].price
                {
                    lit_price = 0;
                } else {
                    let at = self.lit_probs_at(position, self.mf.window()[data - 1]);
                    let probs = &self.lit_probs[at..at + 0x300];
                    lit_price += if is_lit_state(state) {
                        lit_enc_get_price(probs, cur_byte, &self.prob_prices)
                    } else {
                        lit_enc_matched_get_price(probs, cur_byte, match_byte, &self.prob_prices)
                    };
                    if lit_price < self.opt[cur + 1].price {
                        let next = &mut self.opt[cur + 1];
                        next.price = lit_price;
                        next.len = 1;
                        next.make_as_lit();
                        next_is_lit = true;
                    }
                }

                let rep_match_price =
                    match_price + price_1(&self.prob_prices, self.is_rep[state as usize]);

                let mut num_avail_full = self.num_avail;
                {
                    let temp = (K_NUM_OPTS - 1 - cur) as u32;
                    if num_avail_full > temp {
                        num_avail_full = temp;
                    }
                }

                // ---------- SHORT_REP ----------
                if is_lit_state(state)
                    && match_byte == cur_byte
                    && rep_match_price < self.opt[cur + 1].price
                    && (self.opt[cur + 1].len < 2 || self.opt[cur + 1].dist != 0)
                {
                    let short_rep_price =
                        rep_match_price + self.get_price_short_rep(state, pos_state);
                    if short_rep_price < self.opt[cur + 1].price {
                        let next = &mut self.opt[cur + 1];
                        next.price = short_rep_price;
                        next.len = 1;
                        next.make_as_short_rep();
                        next_is_lit = false;
                    }
                }

                if num_avail_full < 2 {
                    continue;
                }
                let num_avail = if num_avail_full <= self.num_fast_bytes {
                    num_avail_full
                } else {
                    self.num_fast_bytes
                };

                // ---------- LIT : REP_0 ----------
                if !next_is_lit && lit_price != 0 && match_byte != cur_byte && num_avail_full > 2 {
                    let data2 = data - reps[0] as usize;
                    let buf = self.mf.window();
                    if buf[data + 1] == buf[data2 + 1] && buf[data + 2] == buf[data2 + 2] {
                        let mut limit = self.num_fast_bytes + 1;
                        if limit > num_avail_full {
                            limit = num_avail_full;
                        }
                        let mut len = 3u32;
                        while len < limit && buf[data + len as usize] == buf[data2 + len as usize] {
                            len += 1;
                        }

                        let state2 = u32::from(K_LITERAL_NEXT_STATES[state as usize]);
                        let pos_state2 = (position + 1) & self.pb_mask;
                        let base = lit_price + self.get_price_rep_0(state2, pos_state2);
                        let offset = cur + len as usize;
                        if last < offset {
                            last = offset;
                        }
                        let len = len - 1;
                        let price2 = base + self.rep_len_enc.get_price_len(pos_state2, len);
                        let opt = &mut self.opt[offset];
                        if price2 < opt.price {
                            opt.price = price2;
                            opt.len = len;
                            opt.dist = 0;
                            opt.extra = 1;
                        }
                    }
                }

                let mut start_len = 2u32; // C: "speed optimization"

                // ---------- REP ----------
                #[allow(clippy::needless_range_loop)]
                for rep_index in 0..LZMA_NUM_REPS {
                    let data2 = data - reps[rep_index] as usize;
                    {
                        let buf = self.mf.window();
                        if buf[data] != buf[data2] || buf[data + 1] != buf[data2 + 1] {
                            continue;
                        }
                    }
                    let mut len = 2u32;
                    {
                        let buf = self.mf.window();
                        while len < num_avail
                            && buf[data + len as usize] == buf[data2 + len as usize]
                        {
                            len += 1;
                        }
                    }

                    {
                        let offset = cur + len as usize;
                        if last < offset {
                            last = offset;
                        }
                    }
                    let mut base =
                        rep_match_price + self.get_price_pure_rep(rep_index, state, pos_state);
                    {
                        let mut len2 = len;
                        loop {
                            let price2 = base + self.rep_len_enc.get_price_len(pos_state, len2);
                            let opt = &mut self.opt[cur + len2 as usize];
                            if price2 < opt.price {
                                opt.price = price2;
                                opt.len = len2;
                                opt.dist = rep_index as u32;
                                opt.extra = 0;
                            }
                            len2 -= 1;
                            if len2 < 2 {
                                break;
                            }
                        }
                    }

                    if rep_index == 0 {
                        start_len = len + 1; // C 17.old
                    }

                    // ---------- REP : LIT : REP_0 ----------
                    let mut len2 = len + 1;
                    let mut limit = len2 + self.num_fast_bytes;
                    if limit > num_avail_full {
                        limit = num_avail_full;
                    }
                    len2 += 2;
                    if len2 <= limit && {
                        let buf = self.mf.window();
                        buf[data + len2 as usize - 2] == buf[data2 + len2 as usize - 2]
                            && buf[data + len2 as usize - 1] == buf[data2 + len2 as usize - 1]
                    } {
                        let state2 = u32::from(K_REP_NEXT_STATES[state as usize]);
                        let mut pos_state2 = (position + len) & self.pb_mask;
                        let lit_at = self.lit_probs_at(
                            position + len,
                            self.mf.window()[data + len as usize - 1],
                        );
                        base += self.rep_len_enc.get_price_len(pos_state, len)
                            + price_0(
                                &self.prob_prices,
                                self.is_match[state2 as usize][pos_state2 as usize],
                            )
                            + lit_enc_matched_get_price(
                                &self.lit_probs[lit_at..lit_at + 0x300],
                                u32::from(self.mf.window()[data + len as usize]),
                                u32::from(self.mf.window()[data2 + len as usize]),
                                &self.prob_prices,
                            );

                        let state2 = K_STATE_LIT_AFTER_REP;
                        pos_state2 = (pos_state2 + 1) & self.pb_mask;
                        base += self.get_price_rep_0(state2, pos_state2);

                        {
                            let buf = self.mf.window();
                            while len2 < limit
                                && buf[data + len2 as usize] == buf[data2 + len2 as usize]
                            {
                                len2 += 1;
                            }
                        }
                        len2 -= len;

                        let offset = cur + len as usize + len2 as usize;
                        if last < offset {
                            last = offset;
                        }
                        let len2 = len2 - 1;
                        let price2 = base + self.rep_len_enc.get_price_len(pos_state2, len2);
                        let opt = &mut self.opt[offset];
                        if price2 < opt.price {
                            opt.price = price2;
                            opt.len = len2;
                            opt.extra = (len + 1) as u16;
                            opt.dist = rep_index as u32;
                        }
                    }
                }

                // ---------- MATCH ----------
                if new_len > num_avail {
                    new_len = num_avail;
                    num_pairs = 0;
                    while new_len > self.matches[num_pairs] {
                        num_pairs += 2;
                    }
                    self.matches[num_pairs] = new_len;
                    num_pairs += 2;
                }

                if new_len >= start_len {
                    let normal_match_price =
                        match_price + price_0(&self.prob_prices, self.is_rep[state as usize]);

                    {
                        let offset = cur + new_len as usize;
                        if last < offset {
                            last = offset;
                        }
                    }

                    let mut offs = 0usize;
                    while start_len > self.matches[offs] {
                        offs += 2;
                    }
                    let mut dist = self.matches[offs + 1];
                    let mut pos_slot = self.get_pos_slot2(dist);

                    let mut len = start_len;
                    loop {
                        let mut base =
                            normal_match_price + self.len_enc.get_price_len(pos_state, len);
                        {
                            let len_norm = get_len_to_pos_state2(len - 2);
                            let price2 = if (dist as usize) < K_NUM_FULL_DISTANCES {
                                base + self.distances_prices[len_norm]
                                    [dist as usize & (K_NUM_FULL_DISTANCES - 1)]
                            } else {
                                base + self.pos_slot_prices[len_norm][pos_slot as usize]
                                    + self.align_prices[(dist & K_ALIGN_MASK) as usize]
                            };
                            let opt = &mut self.opt[cur + len as usize];
                            if price2 < opt.price {
                                opt.price = price2;
                                opt.len = len;
                                opt.dist = dist + LZMA_NUM_REPS as u32;
                                opt.extra = 0;
                            }
                            base = price2;
                        }

                        if len == self.matches[offs] {
                            // ---------- MATCH : LIT : REP_0 ----------
                            let data2 = data - dist as usize - 1;
                            let mut len2 = len + 1;
                            let mut limit = len2 + self.num_fast_bytes;
                            if limit > num_avail_full {
                                limit = num_avail_full;
                            }
                            len2 += 2;
                            if len2 <= limit && {
                                let buf = self.mf.window();
                                buf[data + len2 as usize - 2] == buf[data2 + len2 as usize - 2]
                                    && buf[data + len2 as usize - 1]
                                        == buf[data2 + len2 as usize - 1]
                            } {
                                {
                                    let buf = self.mf.window();
                                    while len2 < limit
                                        && buf[data + len2 as usize] == buf[data2 + len2 as usize]
                                    {
                                        len2 += 1;
                                    }
                                }
                                len2 -= len;

                                let state2 = u32::from(K_MATCH_NEXT_STATES[state as usize]);
                                let mut pos_state2 = (position + len) & self.pb_mask;
                                let lit_at = self.lit_probs_at(
                                    position + len,
                                    self.mf.window()[data + len as usize - 1],
                                );
                                base += price_0(
                                    &self.prob_prices,
                                    self.is_match[state2 as usize][pos_state2 as usize],
                                );
                                base += lit_enc_matched_get_price(
                                    &self.lit_probs[lit_at..lit_at + 0x300],
                                    u32::from(self.mf.window()[data + len as usize]),
                                    u32::from(self.mf.window()[data2 + len as usize]),
                                    &self.prob_prices,
                                );

                                let state2 = K_STATE_LIT_AFTER_MATCH;
                                pos_state2 = (pos_state2 + 1) & self.pb_mask;
                                base += self.get_price_rep_0(state2, pos_state2);

                                let offset = cur + len as usize + len2 as usize;
                                if last < offset {
                                    last = offset;
                                }
                                let len2 = len2 - 1;
                                let price2 =
                                    base + self.rep_len_enc.get_price_len(pos_state2, len2);
                                let opt = &mut self.opt[offset];
                                if price2 < opt.price {
                                    opt.price = price2;
                                    opt.len = len2;
                                    opt.extra = (len + 1) as u16;
                                    opt.dist = dist + LZMA_NUM_REPS as u32;
                                }
                            }

                            offs += 2;
                            if offs == num_pairs {
                                break;
                            }
                            dist = self.matches[offs + 1];
                            pos_slot = self.get_pos_slot2(dist);
                        }
                        len += 1;
                    }
                }
            }
        }

        let mut l = last;
        loop {
            self.opt[l].price = K_INFINITY_PRICE;
            l -= 1;
            if l == 0 {
                break;
            }
        }

        self.backward(cur)
    }

    /// C: `ChangePair`.
    #[inline]
    const fn change_pair(small_dist: u32, big_dist: u32) -> bool {
        (big_dist >> 7) > small_dist
    }

    /// C: `GetOptimumFast`.
    fn get_optimum_fast(&mut self, stream: &mut dyn SeqInStream) -> u32 {
        let (main_len, num_pairs) = if self.additional_offset == 0 {
            self.read_match_distances(stream)
        } else {
            (self.longest_match_len, self.num_pairs)
        };
        let mut main_len = main_len;
        let mut num_pairs = num_pairs;

        let mut num_avail = self.num_avail;
        self.back_res = MARK_LIT;
        if num_avail < 2 {
            return 1;
        }
        if num_avail > LZMA_MATCH_LEN_MAX {
            num_avail = LZMA_MATCH_LEN_MAX;
        }
        let data = self.mf.cur() - 1;
        let mut rep_len = 0u32;
        let mut rep_index = 0usize;

        for i in 0..LZMA_NUM_REPS {
            let data2 = data - self.reps[i] as usize;
            let mut len = 2u32;
            {
                let buf = self.mf.window();
                if buf[data] != buf[data2] || buf[data + 1] != buf[data2 + 1] {
                    continue;
                }
                while len < num_avail && buf[data + len as usize] == buf[data2 + len as usize] {
                    len += 1;
                }
            }
            if len >= self.num_fast_bytes {
                self.back_res = i as u32;
                self.move_pos(stream, len - 1);
                return len;
            }
            if len > rep_len {
                rep_index = i;
                rep_len = len;
            }
        }

        if main_len >= self.num_fast_bytes {
            self.back_res = self.matches[num_pairs - 1] + LZMA_NUM_REPS as u32;
            self.move_pos(stream, main_len - 1);
            return main_len;
        }

        let mut main_dist = 0u32; // C: "for GCC"

        if main_len >= 2 {
            main_dist = self.matches[num_pairs - 1];
            while num_pairs > 2 {
                if main_len != self.matches[num_pairs - 4] + 1 {
                    break;
                }
                let dist2 = self.matches[num_pairs - 3];
                if !Self::change_pair(dist2, main_dist) {
                    break;
                }
                num_pairs -= 2;
                main_len -= 1;
                main_dist = dist2;
            }
            if main_len == 2 && main_dist >= 0x80 {
                main_len = 1;
            }
        }

        if rep_len >= 2
            && (rep_len + 1 >= main_len
                || (rep_len + 2 >= main_len && main_dist >= (1 << 9))
                || (rep_len + 3 >= main_len && main_dist >= (1 << 15)))
        {
            self.back_res = rep_index as u32;
            self.move_pos(stream, rep_len - 1);
            return rep_len;
        }

        if main_len < 2 || num_avail <= 2 {
            return 1;
        }

        {
            let (len1, pairs) = self.read_match_distances(stream);
            self.num_pairs = pairs;
            self.longest_match_len = len1;

            if len1 >= 2 {
                let new_dist = self.matches[self.num_pairs - 1];
                if (len1 >= main_len && new_dist < main_dist)
                    || (len1 == main_len + 1 && !Self::change_pair(main_dist, new_dist))
                    || (len1 > main_len + 1)
                    || (len1 + 1 >= main_len
                        && main_len >= 3
                        && Self::change_pair(new_dist, main_dist))
                {
                    return 1;
                }
            }
        }

        let data = self.mf.cur() - 1;

        for i in 0..LZMA_NUM_REPS {
            let data2 = data - self.reps[i] as usize;
            let buf = self.mf.window();
            if buf[data] != buf[data2] || buf[data + 1] != buf[data2 + 1] {
                continue;
            }
            let limit = main_len - 1;
            let mut len = 2u32;
            loop {
                if len >= limit {
                    return 1;
                }
                if buf[data + len as usize] != buf[data2 + len as usize] {
                    break;
                }
                len += 1;
            }
        }

        self.back_res = main_dist + LZMA_NUM_REPS as u32;
        if main_len != 2 {
            self.move_pos(stream, main_len - 2);
        }
        main_len
    }

    // -----------------------------------------------------------------------
    // Coding.
    // -----------------------------------------------------------------------

    /// C: `WriteEndMarker`.
    fn write_end_marker(&mut self, pos_state: u32, out: &mut dyn SeqOutStream) {
        {
            let mut prob = self.is_match[self.state as usize][pos_state as usize];
            self.rc.encode_bit_1(&mut prob, out);
            self.is_match[self.state as usize][pos_state as usize] = prob;
            let mut prob = self.is_rep[self.state as usize];
            self.rc.encode_bit_0(&mut prob, out);
            self.is_rep[self.state as usize] = prob;
        }
        self.state = u32::from(K_MATCH_NEXT_STATES[self.state as usize]);

        self.len_probs.encode(&mut self.rc, 0, pos_state, out);

        {
            // C: `RcTree_Encode_PosSlot(..., (1 << kNumPosSlotBits) - 1)`.
            let mut m = 1usize;
            loop {
                let mut prob = self.pos_slot_encoder[0][m];
                self.rc.encode_bit_1(&mut prob, out);
                self.pos_slot_encoder[0][m] = prob;
                m = (m << 1) + 1;
                if m >= (1 << K_NUM_POS_SLOT_BITS) {
                    break;
                }
            }
        }
        {
            // C: `RangeEnc_EncodeDirectBits(..., 30 - kNumAlignBits)`, all ones.
            for _ in 0..(30 - K_NUM_ALIGN_BITS) {
                self.rc.encode_direct_bit(u32::MAX, out);
            }
        }
        {
            // C: `RcTree_ReverseEncode(..., kNumAlignBits, kAlignMask)`.
            let mut m = 1usize;
            loop {
                let mut prob = self.pos_align_encoder[m];
                self.rc.encode_bit_1(&mut prob, out);
                self.pos_align_encoder[m] = prob;
                m = (m << 1) + 1;
                if m >= K_ALIGN_TABLE_SIZE {
                    break;
                }
            }
        }
    }

    /// C: `CheckErrors`.
    fn check_errors(&mut self) -> Result<(), Error> {
        self.result?;
        if self.rc.res.is_err() {
            self.result = Err(Error::Write);
        }
        // C: `if (p->mtMode && p->matchFinderMt.failure_LZ_BT) p->result =
        // MY_HRES_ERROR_INTERNAL_ERROR;`
        if self.mf.mt_failed() {
            self.result = Err(Error::InternalFailure);
        }
        if self.mf.result().is_err() {
            self.result = Err(Error::Read);
        }
        if self.result.is_err() {
            self.finished = true;
        }
        self.result
    }

    /// C: `Flush`.
    fn flush(&mut self, now_pos: u32, out: &mut dyn SeqOutStream) -> Result<(), Error> {
        self.finished = true;
        if self.write_end_mark {
            self.write_end_marker(now_pos & self.pb_mask, out);
        }
        self.rc.flush_data(out);
        self.rc.flush_stream(out);
        self.check_errors()
    }

    /// C: `LzmaEnc_CodeOneBlock`.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn code_one_block(
        &mut self,
        stream: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
        max_pack_size: usize,
        max_unpack_size: u32,
    ) -> Result<(), Error> {
        if self.need_init {
            self.mf.init(stream)?;
            self.need_init = false;
        }

        if self.finished {
            return self.result;
        }
        self.check_errors()?;

        let mut now_pos32 = self.now_pos64 as u32;
        let start_pos32 = now_pos32;

        if self.now_pos64 == 0 {
            if self.mf.get_num_available_bytes() == 0 {
                return self.flush(now_pos32, out);
            }
            self.read_match_distances(stream);
            let mut prob = self.is_match[K_STATE_START][0];
            self.rc.encode_bit_0(&mut prob, out);
            self.is_match[K_STATE_START][0] = prob;
            let cur_byte =
                u32::from(self.mf.window()[self.mf.cur() - self.additional_offset as usize]);
            self.rc
                .lit_encode(&mut self.lit_probs[..0x300], cur_byte, out);
            self.additional_offset -= 1;
            now_pos32 += 1;
        }

        if self.mf.get_num_available_bytes() != 0 {
            loop {
                let len = if self.fast_mode {
                    self.get_optimum_fast(stream)
                } else {
                    let oci = self.opt_cur;
                    if self.opt_end == oci {
                        self.get_optimum(stream, now_pos32)
                    } else {
                        let opt = self.opt[oci];
                        self.back_res = opt.dist;
                        self.opt_cur = oci + 1;
                        opt.len
                    }
                };

                let pos_state = now_pos32 & self.pb_mask;
                let mut dist = self.back_res;

                if dist == MARK_LIT {
                    let mut prob = self.is_match[self.state as usize][pos_state as usize];
                    self.rc.encode_bit_0(&mut prob, out);
                    self.is_match[self.state as usize][pos_state as usize] = prob;

                    let data = self.mf.cur() - self.additional_offset as usize;
                    let at = self.lit_probs_at(now_pos32, self.mf.window()[data - 1]);
                    let cur_byte = u32::from(self.mf.window()[data]);
                    let state = self.state;
                    self.state = u32::from(K_LITERAL_NEXT_STATES[state as usize]);
                    if is_lit_state(state) {
                        self.rc
                            .lit_encode(&mut self.lit_probs[at..at + 0x300], cur_byte, out);
                    } else {
                        let match_byte = u32::from(self.mf.window()[data - self.reps[0] as usize]);
                        self.rc.lit_encode_matched(
                            &mut self.lit_probs[at..at + 0x300],
                            cur_byte,
                            match_byte,
                            out,
                        );
                    }
                } else {
                    let mut prob = self.is_match[self.state as usize][pos_state as usize];
                    self.rc.encode_bit_1(&mut prob, out);
                    self.is_match[self.state as usize][pos_state as usize] = prob;

                    if (dist as usize) < LZMA_NUM_REPS {
                        let mut prob = self.is_rep[self.state as usize];
                        self.rc.encode_bit_1(&mut prob, out);
                        self.is_rep[self.state as usize] = prob;

                        if dist == 0 {
                            let mut prob = self.is_rep_g0[self.state as usize];
                            self.rc.encode_bit_0(&mut prob, out);
                            self.is_rep_g0[self.state as usize] = prob;
                            // C takes `probs = &p->isRep0Long[p->state][posState]`
                            // before the short-rep branch changes `p->state`, so
                            // the update lands in the old state's slot.
                            let si = self.state as usize;
                            let mut prob = self.is_rep0_long[si][pos_state as usize];
                            if len != 1 {
                                self.rc.encode_bit_1_base(&mut prob);
                            } else {
                                self.rc.encode_bit_0_base(&mut prob);
                                self.state = u32::from(K_SHORT_REP_NEXT_STATES[si]);
                            }
                            self.is_rep0_long[si][pos_state as usize] = prob;
                        } else {
                            let mut prob = self.is_rep_g0[self.state as usize];
                            self.rc.encode_bit_1(&mut prob, out);
                            self.is_rep_g0[self.state as usize] = prob;
                            let mut prob = self.is_rep_g1[self.state as usize];
                            if dist == 1 {
                                self.rc.encode_bit_0_base(&mut prob);
                                self.is_rep_g1[self.state as usize] = prob;
                                dist = self.reps[1];
                            } else {
                                self.rc.encode_bit_1(&mut prob, out);
                                self.is_rep_g1[self.state as usize] = prob;
                                let mut prob = self.is_rep_g2[self.state as usize];
                                if dist == 2 {
                                    self.rc.encode_bit_0_base(&mut prob);
                                    dist = self.reps[2];
                                } else {
                                    self.rc.encode_bit_1_base(&mut prob);
                                    dist = self.reps[3];
                                    self.reps[3] = self.reps[2];
                                }
                                self.is_rep_g2[self.state as usize] = prob;
                                self.reps[2] = self.reps[1];
                            }
                            self.reps[1] = self.reps[0];
                            self.reps[0] = dist;
                        }

                        self.rc.norm_pub(out);

                        if len != 1 {
                            self.rep_len_probs.encode(
                                &mut self.rc,
                                len - LZMA_MATCH_LEN_MIN,
                                pos_state,
                                out,
                            );
                            self.rep_len_enc_counter -= 1;
                            self.state = u32::from(K_REP_NEXT_STATES[self.state as usize]);
                        }
                    } else {
                        let mut prob = self.is_rep[self.state as usize];
                        self.rc.encode_bit_0(&mut prob, out);
                        self.is_rep[self.state as usize] = prob;
                        self.state = u32::from(K_MATCH_NEXT_STATES[self.state as usize]);

                        self.len_probs.encode(
                            &mut self.rc,
                            len - LZMA_MATCH_LEN_MIN,
                            pos_state,
                            out,
                        );

                        dist -= LZMA_NUM_REPS as u32;
                        self.reps[3] = self.reps[2];
                        self.reps[2] = self.reps[1];
                        self.reps[1] = self.reps[0];
                        self.reps[0] = dist + 1;

                        self.match_price_count += 1;
                        let pos_slot = self.get_pos_slot(dist);
                        {
                            let mut sym = pos_slot + (1 << K_NUM_POS_SLOT_BITS);
                            let lps = get_len_to_pos_state(len);
                            loop {
                                let i = (sym >> K_NUM_POS_SLOT_BITS) as usize;
                                let bit = (sym >> (K_NUM_POS_SLOT_BITS - 1)) & 1;
                                sym <<= 1;
                                let mut prob = self.pos_slot_encoder[lps][i];
                                self.rc.encode_bit(&mut prob, bit, out);
                                self.pos_slot_encoder[lps][i] = prob;
                                if sym >= (1 << (K_NUM_POS_SLOT_BITS * 2)) {
                                    break;
                                }
                            }
                        }

                        if dist >= K_START_POS_MODEL_INDEX {
                            let footer_bits = (pos_slot >> 1) - 1;

                            if (dist as usize) < K_NUM_FULL_DISTANCES {
                                let base = ((2 | (pos_slot & 1)) << footer_bits) as usize;
                                self.rc.rc_tree_reverse_encode(
                                    &mut self.pos_encoders[base..],
                                    footer_bits,
                                    dist,
                                    out,
                                );
                            } else {
                                let mut pos2 = (dist | 0xF) << (32 - footer_bits);
                                loop {
                                    self.rc
                                        .encode_direct_bit(0u32.wrapping_sub(pos2 >> 31), out);
                                    pos2 = pos2.wrapping_add(pos2);
                                    if pos2 == 0xF000_0000 {
                                        break;
                                    }
                                }

                                let mut dist = dist;
                                let mut m = 1usize;
                                for k in 0..4 {
                                    let bit = dist & 1;
                                    if k != 3 {
                                        dist >>= 1;
                                    }
                                    let mut prob = self.pos_align_encoder[m];
                                    self.rc.encode_bit(&mut prob, bit, out);
                                    self.pos_align_encoder[m] = prob;
                                    m = (m << 1) + bit as usize;
                                }
                            }
                        }
                    }
                }

                now_pos32 += len;
                self.additional_offset -= len;

                if self.additional_offset == 0 {
                    if !self.fast_mode {
                        if self.match_price_count >= 64 {
                            self.fill_align_prices();
                            self.fill_distances_prices();
                            let pb = 1usize << self.pb;
                            self.len_enc
                                .update_tables(pb, &self.len_probs, &self.prob_prices);
                        }
                        if self.rep_len_enc_counter <= 0 {
                            self.rep_len_enc_counter = REP_LEN_COUNT;
                            let pb = 1usize << self.pb;
                            self.rep_len_enc.update_tables(
                                pb,
                                &self.rep_len_probs,
                                &self.prob_prices,
                            );
                        }
                    }

                    if self.mf.get_num_available_bytes() == 0 {
                        break;
                    }
                    let processed = now_pos32 - start_pos32;

                    if max_pack_size != 0 {
                        if processed as usize + K_NUM_OPTS + 300 >= max_unpack_size as usize
                            || self.rc.get_processed() as usize + K_PACK_RESERVE >= max_pack_size
                        {
                            break;
                        }
                    } else if processed >= (1 << 17) {
                        self.now_pos64 += u64::from(now_pos32 - start_pos32);
                        return self.check_errors();
                    }
                }
            }
        }

        self.now_pos64 += u64::from(now_pos32 - start_pos32);
        self.flush(now_pos32, out)
    }

    // -----------------------------------------------------------------------
    // Allocation and initialization.
    // -----------------------------------------------------------------------

    /// Whether the threaded match finder is the one this configuration uses.
    ///
    /// C: `p->mtMode = (p->multiThread && !p->fastMode && (MFB.btMode != 0))`.
    /// Never true without `std`, where there is no threaded finder.
    fn mt_mode(&mut self) -> bool {
        cfg!(feature = "std")
            && self.multi_thread
            && !self.fast_mode
            && self.mf.cfg().kind.bt_mode()
    }

    /// C: `LzmaEnc_Alloc`. `data_limit` is [`LzmaEnc::set_data_limit`]'s
    /// promise for this stream.
    fn alloc(&mut self, keep_window_size: u32, data_limit: u64) -> Result<(), Error> {
        {
            let lclp = self.lc + self.lp;
            if self.lit_probs.is_empty() || self.lclp != lclp {
                let n = 0x300usize << lclp;
                self.lit_probs = Vec::new();
                self.lit_probs
                    .try_reserve_exact(n)
                    .map_err(|_| Error::Alloc)?;
                self.lit_probs.resize(n, 0);
                self.save_state.lit_probs = Vec::new();
                self.save_state
                    .lit_probs
                    .try_reserve_exact(n)
                    .map_err(|_| Error::Alloc)?;
                self.save_state.lit_probs.resize(n, 0);
                self.lclp = lclp;
            }
        }

        #[cfg(feature = "std")]
        {
            if self.mt_mode() {
                self.mf.make_mt();
            } else {
                self.mf.make_st();
            }
        }

        self.mf.cfg().big_hash = self.dict_size > K_BIG_HASH_DIC_LIMIT;

        let mut before_size = K_NUM_OPTS as u32;
        let mut dict_size = self.dict_size;
        if dict_size == (2u32 << 30) || dict_size == (3u32 << 30) {
            // C 21.03: keeps 32-bit back distances out of the decoder and
            // removes a useless final normalization.
            dict_size -= 1;
        }

        // The keep window is how far back `Lzma2Enc` may reach to store a
        // chunk it could not compress, and a chunk is never longer than what
        // the stream has supplied.
        let keep_window_size = u64::from(keep_window_size).min(data_limit) as u32;
        if before_size + dict_size < keep_window_size {
            before_size = keep_window_size - dict_size;
        }

        // C: "in worst case we can look ahead for
        //     max(LZMA_MATCH_LEN_MAX, numFastBytes + 1 + numFastBytes) bytes."
        self.mf.create(
            dict_size,
            before_size,
            self.num_fast_bytes,
            LZMA_MATCH_LEN_MAX + 1,
            data_limit,
        )
    }

    /// Whether `LzmaEnc_Alloc` would accept this configuration, without
    /// allocating anything: the errors of `MatchFinder_Create`'s arithmetic,
    /// for a caller that wants a bad setting refused before the first byte.
    pub(crate) fn check_alloc(&mut self, keep_window_size: u32) -> Result<(), Error> {
        self.mf.cfg().big_hash = self.dict_size > K_BIG_HASH_DIC_LIMIT;
        let mut before_size = K_NUM_OPTS as u32;
        let mut dict_size = self.dict_size;
        if dict_size == (2u32 << 30) || dict_size == (3u32 << 30) {
            dict_size -= 1;
        }
        if before_size + dict_size < keep_window_size {
            before_size = keep_window_size - dict_size;
        }
        let mt = self.mt_mode();
        self.mf
            .mem_usage(
                mt,
                dict_size,
                before_size,
                self.num_fast_bytes,
                LZMA_MATCH_LEN_MAX + 1,
            )
            .map(|_| ())
    }

    /// What one encoder of this configuration is estimated to need, in bytes:
    /// the match finder's window and reference tables, the threaded finder's
    /// `hashBuf` and `btBuf` when that is the finder `LzmaEnc_Alloc` will
    /// pick, plus the literal probability arrays. Nothing is allocated.
    ///
    /// C: 7-Zip computes the same quantity outside `C/` to reduce the block
    /// thread count to a memory budget; the arithmetic here is this port's own
    /// allocation sites, not a formula copied from there.
    pub(crate) fn mem_usage(&mut self) -> u64 {
        self.mf.cfg().big_hash = self.dict_size > K_BIG_HASH_DIC_LIMIT;
        let lit_probs = (0x300u64 << (self.lc + self.lp)) * 2 * 2;

        let mut before_size = K_NUM_OPTS as u32;
        let mut dict_size = self.dict_size;
        if dict_size == (2u32 << 30) || dict_size == (3u32 << 30) {
            dict_size -= 1;
        }
        // C: `LZMA2_KEEP_WINDOW_SIZE`, what `Lzma2Enc` prepares with.
        let keep_window_size: u32 = 1 << 21;
        if before_size + dict_size < keep_window_size {
            before_size = keep_window_size - dict_size;
        }
        let mt = self.mt_mode();
        let mf = self
            .mf
            .mem_usage(
                mt,
                dict_size,
                before_size,
                self.num_fast_bytes,
                LZMA_MATCH_LEN_MAX + 1,
            )
            .unwrap_or(0);
        mf.saturating_add(lit_probs)
    }

    /// What this encoder has allocated, in bytes: the match finder's buffers
    /// and both literal probability arrays.
    #[cfg(test)]
    pub(crate) fn allocated(&self) -> u64 {
        let lit = self.lit_probs.len() + self.save_state.lit_probs.len();
        self.mf.allocated() + (lit * core::mem::size_of::<u16>()) as u64
    }

    /// C: `LzmaEnc_Init`.
    fn init_state(&mut self) {
        self.state = 0;
        self.reps = [1; LZMA_NUM_REPS];

        self.rc.init();

        self.pos_align_encoder.fill(K_PROB_INIT_VALUE);
        for i in 0..K_NUM_STATES as usize {
            self.is_match[i].fill(K_PROB_INIT_VALUE);
            self.is_rep0_long[i].fill(K_PROB_INIT_VALUE);
            self.is_rep[i] = K_PROB_INIT_VALUE;
            self.is_rep_g0[i] = K_PROB_INIT_VALUE;
            self.is_rep_g1[i] = K_PROB_INIT_VALUE;
            self.is_rep_g2[i] = K_PROB_INIT_VALUE;
        }
        for probs in &mut self.pos_slot_encoder {
            probs.fill(K_PROB_INIT_VALUE);
        }
        self.pos_encoders.fill(K_PROB_INIT_VALUE);

        let num = 0x300usize << (self.lp + self.lc);
        self.lit_probs[..num].fill(K_PROB_INIT_VALUE);

        self.len_probs.init();
        self.rep_len_probs.init();

        self.opt_end = 0;
        self.opt_cur = 0;
        for opt in &mut self.opt {
            opt.price = K_INFINITY_PRICE;
        }

        self.additional_offset = 0;

        self.pb_mask = (1u32 << self.pb) - 1;
        self.lp_mask = (0x100u32 << self.lp) - (0x100u32 >> self.lc);
    }

    /// C: `LzmaEnc_InitPrices`.
    pub(crate) fn init_prices(&mut self) {
        if !self.fast_mode {
            self.fill_distances_prices();
            self.fill_align_prices();
        }

        let table_size = (self.num_fast_bytes + 1 - LZMA_MATCH_LEN_MIN) as usize;
        self.len_enc.table_size = table_size;
        self.rep_len_enc.table_size = table_size;

        self.rep_len_enc_counter = REP_LEN_COUNT;

        let pb = 1usize << self.pb;
        self.len_enc
            .update_tables(pb, &self.len_probs, &self.prob_prices);
        self.rep_len_enc
            .update_tables(pb, &self.rep_len_probs, &self.prob_prices);
    }

    /// C: `LzmaEnc_AllocAndInit`.
    fn alloc_and_init(&mut self, keep_window_size: u32) -> Result<(), Error> {
        let mut i = (K_END_POS_MODEL_INDEX / 2) as usize;
        while (i as u32) < K_DIC_LOG_SIZE_MAX {
            if self.dict_size <= (1u32 << i) {
                break;
            }
            i += 1;
        }
        self.dist_table_size = i * 2;

        self.finished = false;
        self.result = Ok(());
        self.now_pos64 = 0;
        self.need_init = true;
        // The promise is for this stream alone.
        let data_limit = core::mem::replace(&mut self.data_limit, u64::MAX);
        self.alloc(keep_window_size, data_limit)?;
        self.init_state();
        self.init_prices();
        Ok(())
    }

    /// C: `LzmaEnc_Prepare` / `LzmaEnc_PrepareForLzma2`. The stream itself is
    /// passed to [`Self::code_one_block`] rather than stored.
    pub(crate) fn prepare(&mut self, keep_window_size: u32) -> Result<(), Error> {
        self.alloc_and_init(keep_window_size)
    }

    /// C: `LzmaEnc_MemPrepare`, whose `MatchFinder_SET_DIRECT_INPUT_BUF` also
    /// sets `expectedDataSize` from the source length.
    pub(crate) fn mem_prepare(&mut self, src_len: u64, keep_window_size: u32) -> Result<(), Error> {
        self.set_data_size(src_len);
        self.alloc_and_init(keep_window_size)
    }

    /// The threaded match finder's handle, when one is in use.
    #[cfg(feature = "std")]
    pub(crate) fn mt_handle(&self) -> Option<alloc::sync::Arc<crate::enc::lz_find_mt::MtShared>> {
        self.mf.mt_handle()
    }

    /// C: `LzmaEnc_GetCurBuf`, as an offset into the match finder's window.
    ///
    pub(crate) fn get_cur_buf(&self) -> usize {
        self.mf.cur() - self.additional_offset as usize
    }

    /// The match finder's window, which `get_cur_buf` indexes.
    pub(crate) fn window(&self) -> &[u8] {
        self.mf.window()
    }

    /// C: `LzmaEnc_SaveState`, through `COPY_LZMA_ENC_STATE`.
    pub(crate) fn save_state(&mut self) {
        let v = &mut self.save_state;
        v.state = self.state;
        v.reps = self.reps;
        v.pos_align_encoder = self.pos_align_encoder;
        v.is_rep = self.is_rep;
        v.is_rep_g0 = self.is_rep_g0;
        v.is_rep_g1 = self.is_rep_g1;
        v.is_rep_g2 = self.is_rep_g2;
        v.is_match = self.is_match;
        v.is_rep0_long = self.is_rep0_long;
        v.pos_slot_encoder = self.pos_slot_encoder;
        v.pos_encoders = self.pos_encoders;
        v.len_probs = self.len_probs.clone();
        v.rep_len_probs = self.rep_len_probs.clone();
        let n = 0x300usize << self.lclp;
        v.lit_probs[..n].copy_from_slice(&self.lit_probs[..n]);
    }

    /// C: `LzmaEnc_RestoreState`.
    pub(crate) fn restore_state(&mut self) {
        let v = &self.save_state;
        self.state = v.state;
        self.reps = v.reps;
        self.pos_align_encoder = v.pos_align_encoder;
        self.is_rep = v.is_rep;
        self.is_rep_g0 = v.is_rep_g0;
        self.is_rep_g1 = v.is_rep_g1;
        self.is_rep_g2 = v.is_rep_g2;
        self.is_match = v.is_match;
        self.is_rep0_long = v.is_rep0_long;
        self.pos_slot_encoder = v.pos_slot_encoder;
        self.pos_encoders = v.pos_encoders;
        self.len_probs = v.len_probs.clone();
        self.rep_len_probs = v.rep_len_probs.clone();
        let n = 0x300usize << self.lclp;
        let (lit, save) = (&mut self.lit_probs, &self.save_state.lit_probs);
        lit[..n].copy_from_slice(&save[..n]);
    }

    /// C: `LzmaEnc_CodeOneMemBlock`, which re-initializes the range encoder
    /// for one LZMA2 chunk and reports how much it packed and unpacked.
    pub(crate) fn code_one_mem_block(
        &mut self,
        stream: &mut dyn SeqInStream,
        re_init: bool,
        dest: &mut Vec<u8>,
        dest_limit: usize,
        desired_pack_size: usize,
        unpack_size: &mut u32,
    ) -> Result<bool, Error> {
        let now_pos64 = self.now_pos64;

        self.write_end_mark = false;
        self.finished = false;
        self.result = Ok(());

        if re_init {
            self.init_state();
        }
        self.init_prices();
        self.rc.init();

        let mut sink = LimitedSink {
            out: dest,
            rem: dest_limit,
            overflow: false,
        };
        let res = self.code_one_block(stream, &mut sink, desired_pack_size, *unpack_size);
        let overflow = sink.overflow;

        *unpack_size = (self.now_pos64 - now_pos64) as u32;
        if overflow {
            return Ok(true);
        }
        res.map(|()| false)
    }
}

/// C: `CLzmaEnc_SeqOutStreamBuf`, the bounded buffer `LzmaEnc_CodeOneMemBlock`
/// writes into, which records an overflow rather than failing at once.
struct LimitedSink<'a> {
    out: &'a mut Vec<u8>,
    rem: usize,
    overflow: bool,
}

impl SeqOutStream for LimitedSink<'_> {
    fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut size = data.len();
        if self.rem < size {
            size = self.rem;
            self.overflow = true;
        }
        if size != 0 {
            self.out.try_reserve(size).map_err(|_| Error::Alloc)?;
            self.out.extend_from_slice(&data[..size]);
            self.rem -= size;
        }
        Ok(())
    }
}

/// C: `LzmaEnc_FastPosInit`.
fn fast_pos_init() -> Vec<u8> {
    let mut g = vec![0u8; 1 << K_NUM_LOG_BITS];
    g[0] = 0;
    g[1] = 1;
    let mut at = 2usize;
    for slot in 2..K_NUM_LOG_BITS * 2 {
        let k = 1usize << ((slot >> 1) - 1);
        for j in 0..k {
            g[at + j] = slot as u8;
        }
        at += k;
    }
    g
}
