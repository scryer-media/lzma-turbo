//! Price tables and the length coder.
//!
//! C: `LzmaEnc_InitPriceTables`, the `GET_PRICE*` macros, `CLenEnc` /
//! `CLenPriceEnc` and `LenPriceEnc_UpdateTables` in `C/LzmaEnc.c`.
//!
//! A price is a bit count scaled by `1 << kNumBitPriceShiftBits`, so the
//! optimal parser can add the cost of a symbol without encoding it.

use crate::enc::consts::*;
use crate::enc::range_enc::RangeEnc;
use crate::enc::stream::SeqOutStream;

/// C: `CProbPrice ProbPrices[kBitModelTotal >> kNumMoveReducingBits]`.
pub(crate) type ProbPrices = [u32; (K_BIT_MODEL_TOTAL >> K_NUM_MOVE_REDUCING_BITS) as usize];

/// A probability is below `kBitModelTotal`, so its price index already fits
/// [`ProbPrices`]; masking it says so to the compiler, which then drops the
/// bounds check from every price lookup. The mask never changes an index.
const PP_MASK: usize = (K_BIT_MODEL_TOTAL >> K_NUM_MOVE_REDUCING_BITS) as usize - 1;

/// C: `LzmaEnc_InitPriceTables`.
pub(crate) fn init_price_tables() -> ProbPrices {
    let mut prices: ProbPrices = [0; (K_BIT_MODEL_TOTAL >> K_NUM_MOVE_REDUCING_BITS) as usize];
    for (i, slot) in prices.iter_mut().enumerate() {
        let k_cycles_bits = K_NUM_BIT_PRICE_SHIFT_BITS;
        let mut w =
            ((i as u32) << K_NUM_MOVE_REDUCING_BITS) + (1 << (K_NUM_MOVE_REDUCING_BITS - 1));
        let mut bit_count = 0u32;
        for _ in 0..k_cycles_bits {
            w = w.wrapping_mul(w);
            bit_count <<= 1;
            while w >= (1 << 16) {
                w >>= 1;
                bit_count += 1;
            }
        }
        *slot = (K_NUM_BIT_MODEL_TOTAL_BITS << k_cycles_bits) - 15 - bit_count;
    }
    prices
}

/// C: `GET_PRICEa(prob, bit)`.
#[inline]
pub(crate) fn price(pp: &ProbPrices, prob: u16, bit: u32) -> u32 {
    let masked = u32::from(prob) ^ (0u32.wrapping_sub(bit) & (K_BIT_MODEL_TOTAL - 1));
    pp[(masked >> K_NUM_MOVE_REDUCING_BITS) as usize & PP_MASK]
}

/// C: `GET_PRICEa_0(prob)`.
#[inline]
pub(crate) fn price_0(pp: &ProbPrices, prob: u16) -> u32 {
    pp[(u32::from(prob) >> K_NUM_MOVE_REDUCING_BITS) as usize & PP_MASK]
}

/// C: `GET_PRICEa_1(prob)`.
#[inline]
pub(crate) fn price_1(pp: &ProbPrices, prob: u16) -> u32 {
    pp[((u32::from(prob) ^ (K_BIT_MODEL_TOTAL - 1)) >> K_NUM_MOVE_REDUCING_BITS) as usize & PP_MASK]
}

/// C: `LitEnc_GetPrice`.
pub(crate) fn lit_enc_get_price(probs: &[u16], sym: u32, pp: &ProbPrices) -> u32 {
    let mut total = 0u32;
    let mut sym = sym | 0x100;
    loop {
        let bit = sym & 1;
        sym >>= 1;
        total += price(pp, probs[sym as usize], bit);
        if sym < 2 {
            break;
        }
    }
    total
}

/// C: `LitEnc_Matched_GetPrice`.
pub(crate) fn lit_enc_matched_get_price(
    probs: &[u16],
    sym: u32,
    match_byte: u32,
    pp: &ProbPrices,
) -> u32 {
    let mut total = 0u32;
    let mut offs = 0x100u32;
    let mut sym = sym | 0x100;
    let mut match_byte = match_byte;
    loop {
        match_byte <<= 1;
        total += price(
            pp,
            probs[(offs + (match_byte & offs) + (sym >> 8)) as usize],
            (sym >> 7) & 1,
        );
        sym <<= 1;
        offs &= !(match_byte ^ sym);
        if sym >= 0x10000 {
            break;
        }
    }
    total
}

/// C: `CLenEnc`.
#[derive(Clone)]
pub(crate) struct LenEnc {
    pub(crate) low: [u16; LZMA_NUM_PB_STATES_MAX << (K_LEN_NUM_LOW_BITS + 1)],
    pub(crate) high: [u16; K_LEN_NUM_HIGH_SYMBOLS as usize],
}

impl LenEnc {
    /// C: `LenEnc_Init`.
    pub(crate) fn new() -> Self {
        LenEnc {
            low: [K_PROB_INIT_VALUE; LZMA_NUM_PB_STATES_MAX << (K_LEN_NUM_LOW_BITS + 1)],
            high: [K_PROB_INIT_VALUE; K_LEN_NUM_HIGH_SYMBOLS as usize],
        }
    }

    /// C: `LenEnc_Init`.
    pub(crate) fn init(&mut self) {
        self.low.fill(K_PROB_INIT_VALUE);
        self.high.fill(K_PROB_INIT_VALUE);
    }

    /// C: `LenEnc_Encode`.
    pub(crate) fn encode(
        &mut self,
        rc: &mut RangeEnc,
        mut sym: u32,
        pos_state: u32,
        out: &mut dyn SeqOutStream,
    ) {
        // C: `probs` walks `p->low`; here it is the index of its first entry.
        let mut base = 0usize;
        if sym >= K_LEN_NUM_LOW_SYMBOLS {
            let mut prob = self.low[base];
            rc.encode_bit_1(&mut prob, out);
            self.low[base] = prob;
            base += K_LEN_NUM_LOW_SYMBOLS as usize;
            if sym >= K_LEN_NUM_LOW_SYMBOLS * 2 {
                let mut prob = self.low[base];
                rc.encode_bit_1(&mut prob, out);
                self.low[base] = prob;
                // C: `LitEnc_Encode(rc, p->high, sym - kLenNumLowSymbols * 2)`.
                rc.lit_encode(&mut self.high, sym - K_LEN_NUM_LOW_SYMBOLS * 2, out);
                return;
            }
            sym -= K_LEN_NUM_LOW_SYMBOLS;
        }

        // C: `RcTree_Encode(rc, probs + (posState << kLenNumLowBits), kLenNumLowBits, sym)`,
        // unrolled as the C unrolls it.
        let mut prob = self.low[base];
        rc.encode_bit_0(&mut prob, out);
        self.low[base] = prob;
        base += (pos_state as usize) << (1 + K_LEN_NUM_LOW_BITS);

        let bit = sym >> 2;
        let mut prob = self.low[base + 1];
        rc.encode_bit(&mut prob, bit, out);
        self.low[base + 1] = prob;
        let mut m = (1usize << 1) + bit as usize;

        let bit = (sym >> 1) & 1;
        let mut prob = self.low[base + m];
        rc.encode_bit(&mut prob, bit, out);
        self.low[base + m] = prob;
        m = (m << 1) + bit as usize;

        let bit = sym & 1;
        let mut prob = self.low[base + m];
        rc.encode_bit(&mut prob, bit, out);
        self.low[base + m] = prob;
    }
}

/// C: `CLenPriceEnc`.
pub(crate) struct LenPriceEnc {
    pub(crate) table_size: usize,
    pub(crate) prices: [[u32; K_LEN_NUM_SYMBOLS_TOTAL]; LZMA_NUM_PB_STATES_MAX],
}

impl LenPriceEnc {
    pub(crate) fn new() -> Self {
        LenPriceEnc {
            table_size: 0,
            prices: [[0; K_LEN_NUM_SYMBOLS_TOTAL]; LZMA_NUM_PB_STATES_MAX],
        }
    }

    /// C: `GET_PRICE_LEN(p, posState, len)`.
    #[inline]
    pub(crate) fn get_price_len(&self, pos_state: u32, len: u32) -> u32 {
        self.prices[pos_state as usize][(len - LZMA_MATCH_LEN_MIN) as usize]
    }

    /// C: `LenPriceEnc_UpdateTables`.
    pub(crate) fn update_tables(&mut self, num_pos_states: usize, enc: &LenEnc, pp: &ProbPrices) {
        let mut b;
        {
            let prob = enc.low[0];
            b = price_1(pp, prob);
            let a = price_0(pp, prob);
            let c = b + price_0(pp, enc.low[K_LEN_NUM_LOW_SYMBOLS as usize]);
            for pos_state in 0..num_pos_states {
                let pos_state2 = pos_state << (1 + K_LEN_NUM_LOW_BITS);
                set_prices_3(&enc.low[pos_state2..], a, &mut self.prices[pos_state], pp);
                let low = K_LEN_NUM_LOW_SYMBOLS as usize;
                set_prices_3(
                    &enc.low[pos_state2 + low..],
                    c,
                    &mut self.prices[pos_state][low..],
                    pp,
                );
            }
        }

        let mut i = self.table_size;
        if i > K_LEN_NUM_LOW_SYMBOLS as usize * 2 {
            let probs = &enc.high;
            let from = K_LEN_NUM_LOW_SYMBOLS as usize * 2;
            i -= K_LEN_NUM_LOW_SYMBOLS as usize * 2 - 1;
            i >>= 1;
            b += price_1(pp, enc.low[K_LEN_NUM_LOW_SYMBOLS as usize]);
            loop {
                i -= 1;
                let mut sym = i + (1 << (K_LEN_NUM_HIGH_BITS - 1));
                let mut total = b;
                loop {
                    let bit = (sym & 1) as u32;
                    sym >>= 1;
                    total += price(pp, probs[sym], bit);
                    if sym < 2 {
                        break;
                    }
                }
                let prob = probs[i + (1 << (K_LEN_NUM_HIGH_BITS - 1))];
                self.prices[0][from + i * 2] = total + price_0(pp, prob);
                self.prices[0][from + i * 2 + 1] = total + price_1(pp, prob);
                if i == 0 {
                    break;
                }
            }

            let num = self.table_size - K_LEN_NUM_LOW_SYMBOLS as usize * 2;
            let (head, tail) = self.prices.split_at_mut(1);
            for prices in tail.iter_mut().take(num_pos_states.saturating_sub(1)) {
                prices[from..from + num].copy_from_slice(&head[0][from..from + num]);
            }
        }
    }
}

/// C: `SetPrices_3`.
fn set_prices_3(probs: &[u16], start_price: u32, prices: &mut [u32], pp: &ProbPrices) {
    let mut i = 0usize;
    while i < 8 {
        let mut total = start_price;
        total += price(pp, probs[1], (i >> 2) as u32);
        total += price(pp, probs[2 + (i >> 2)], ((i >> 1) & 1) as u32);
        let prob = probs[4 + (i >> 1)];
        prices[i] = total + price_0(pp, prob);
        prices[i + 1] = total + price_1(pp, prob);
        i += 2;
    }
}
