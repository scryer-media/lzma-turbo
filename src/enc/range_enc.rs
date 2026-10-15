//! The range encoder.
//!
//! C: `CRangeEnc` and the `RC_*` macros in `C/LzmaEnc.c`.
//!
//! Symbol routines keep `range` in a local across RC_BIT operations,
//! as the C does, and write it back at the SDK's group boundaries. ShiftLow only
//! changes the low/output state, so it cannot force range through memory.
//! Callers pass the actual probability cell, as the SDK does. RC_BIT writes
//! it before normalization, avoiding a deferred copy-back across ShiftLow.

use alloc::vec::Vec;

use crate::enc::consts::*;
use crate::enc::stream::SeqOutStream;
use crate::error::Error;

/// C: CRangeEnc's output-stream handle. A stable address for the borrowed
/// stream gives ShiftLow one output-handle pointer across a literal's bits.
/// This binding lives on the stack and borrows the stream for the call.
pub(super) struct RangeOutput<'a>(pub(super) &'a mut dyn SeqOutStream);

/// C: `CRangeEnc`.
pub(crate) struct RangeEnc {
    pub(crate) range: u32,
    cache: u32,
    low: u64,
    cache_size: u64,
    /// C: `bufBase`/`buf`/`bufLim`. The `Vec`'s length is the C's `buf`
    /// pointer and its capacity is `RC_BUF_SIZE`, the C's `bufLim`.
    buf: Vec<u8>,
    processed: u64,
    pub(crate) res: Result<(), Error>,
}

impl RangeEnc {
    /// C: `RangeEnc_Construct` plus `RangeEnc_Alloc`.
    pub(crate) fn new() -> Result<Self, Error> {
        let mut buf = Vec::new();
        buf.try_reserve_exact(RC_BUF_SIZE)
            .map_err(|_| Error::Alloc)?;
        Ok(RangeEnc {
            range: 0xFFFF_FFFF,
            cache: 0,
            low: 0,
            cache_size: 0,
            buf,
            processed: 0,
            res: Ok(()),
        })
    }

    /// C: `RangeEnc_Init`.
    pub(crate) fn init(&mut self) {
        self.range = 0xFFFF_FFFF;
        self.cache = 0;
        self.low = 0;
        self.cache_size = 0;
        self.buf.clear();
        self.processed = 0;
        self.res = Ok(());
    }

    /// C: `RangeEnc_GetProcessed`.
    pub(crate) fn get_processed(&self) -> u64 {
        self.processed + self.buf.len() as u64 + self.cache_size
    }

    /// C: `RangeEnc_FlushStream`.
    pub(crate) fn flush_stream(&mut self, out: &mut dyn SeqOutStream) {
        let num = self.buf.len();
        if self.res.is_ok()
            && let Err(e) = out.write(&self.buf)
        {
            self.res = Err(e);
        }
        self.processed += num as u64;
        self.buf.clear();
    }

    /// C: `RangeEnc_ShiftLow`.
    fn shift_low(&mut self, out: &mut RangeOutput<'_>) {
        let low = self.low as u32;
        let mut high = (self.low >> 32) as u32;
        self.low = u64::from(low << 8);
        if low < 0xFF00_0000 || high != 0 {
            self.push(self.cache.wrapping_add(high) as u8, out);
            self.cache = low >> 24;
            if self.cache_size == 0 {
                return;
            }
            high = high.wrapping_add(0xFF);
            loop {
                self.push(high as u8, out);
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    return;
                }
            }
        }
        self.cache_size += 1;
    }

    #[inline]
    fn push(&mut self, byte: u8, out: &mut RangeOutput<'_>) {
        self.buf.push(byte);
        if self.buf.len() == RC_BUF_SIZE {
            self.flush_stream(out.0);
        }
    }

    /// C: `RangeEnc_FlushData`.
    pub(crate) fn flush_data(&mut self, out: &mut dyn SeqOutStream) {
        let mut bound = RangeOutput(out);
        for _ in 0..5 {
            self.shift_low(&mut bound);
        }
    }

    /// C: `RC_NORM`.
    #[inline]
    pub(super) fn norm_with_range(&mut self, mut range: u32, out: &mut RangeOutput<'_>) -> u32 {
        if range < K_TOP_VALUE {
            range <<= 8;
            self.shift_low(out);
        }
        range
    }

    /// C: `RC_BIT`, selecting the bound or remainder directly while retaining
    /// its branchless low/probability updates. Every caller supplies one bit.
    #[inline(always)]
    pub(super) fn encode_bit_with_range(
        &mut self,
        mut range: u32,
        prob: &mut u16,
        bit: u32,
        out: &mut RangeOutput<'_>,
    ) -> u32 {
        // C: `RC_BIT_PRE`.
        let ttt = u32::from(*prob);
        let new_bound = (range >> K_NUM_BIT_MODEL_TOTAL_BITS) * ttt;
        let mask = 0u32.wrapping_sub(bit);
        range = if bit == 0 {
            new_bound
        } else {
            range.wrapping_sub(new_bound)
        };
        self.low = self.low.wrapping_add(u64::from(new_bound & mask));
        let mut mask = bit.wrapping_sub(1);
        mask &= K_BIT_MODEL_TOTAL - ((1 << K_NUM_MOVE_BITS) - 1);
        mask = mask.wrapping_add((1 << K_NUM_MOVE_BITS) - 1);
        let ttt = ttt.wrapping_add(((mask.wrapping_sub(ttt)) as i32 >> K_NUM_MOVE_BITS) as u32);
        *prob = ttt as u16;
        if range < K_TOP_VALUE {
            range <<= 8;
            self.shift_low(out);
        }
        range
    }

    /// C: `RC_BIT_0_BASE`.
    #[inline]
    pub(super) fn encode_bit_0_base_with_range(&mut self, range: u32, prob: &mut u16) -> u32 {
        let ttt = u32::from(*prob);
        let new_bound = (range >> K_NUM_BIT_MODEL_TOTAL_BITS) * ttt;
        *prob = (ttt + ((K_BIT_MODEL_TOTAL - ttt) >> K_NUM_MOVE_BITS)) as u16;
        new_bound
    }

    /// C: `RC_BIT_1_BASE`.
    #[inline]
    pub(super) fn encode_bit_1_base_with_range(&mut self, range: u32, prob: &mut u16) -> u32 {
        let ttt = u32::from(*prob);
        let new_bound = (range >> K_NUM_BIT_MODEL_TOTAL_BITS) * ttt;
        self.low += u64::from(new_bound);
        *prob = (ttt - (ttt >> K_NUM_MOVE_BITS)) as u16;
        range - new_bound
    }

    /// C: `RC_BIT_0`, which is `RC_BIT_0_BASE` then `RC_NORM`.
    #[inline]
    pub(super) fn encode_bit_0_with_range(
        &mut self,
        range: u32,
        prob: &mut u16,
        out: &mut RangeOutput<'_>,
    ) -> u32 {
        let range = self.encode_bit_0_base_with_range(range, prob);
        self.norm_with_range(range, out)
    }

    /// C: `RC_BIT_1`.
    #[inline]
    pub(super) fn encode_bit_1_with_range(
        &mut self,
        range: u32,
        prob: &mut u16,
        out: &mut RangeOutput<'_>,
    ) -> u32 {
        let range = self.encode_bit_1_base_with_range(range, prob);
        self.norm_with_range(range, out)
    }

    /// C: `RangeEnc_EncodeBit_0`, used for the first literal's match flag.
    #[inline]
    pub(crate) fn encode_bit_0(&mut self, prob: &mut u16, out: &mut dyn SeqOutStream) {
        self.range = self.encode_bit_0_with_range(self.range, prob, &mut RangeOutput(out));
    }

    /// C: the inlined `RangeEnc_EncodeDirectBits` in `LzmaEnc_CodeOneBlock`'s
    /// far-distance branch and in `WriteEndMarker`.
    #[inline]
    pub(super) fn encode_direct_bit_with_range(
        &mut self,
        range: u32,
        add: u32,
        out: &mut RangeOutput<'_>,
    ) -> u32 {
        let range = range >> 1;
        self.low += u64::from(range & add);
        self.norm_with_range(range, out)
    }

    /// C: `LitEnc_Encode`.
    pub(crate) fn lit_encode(&mut self, probs: &mut [u16], sym: u32, out: &mut dyn SeqOutStream) {
        let mut bound = RangeOutput(out);
        let mut range = self.range;
        let mut sym = sym | 0x100;
        loop {
            let i = (sym >> 8) as usize;
            let bit = (sym >> 7) & 1;
            sym <<= 1;
            let prob = &mut probs[i];
            range = self.encode_bit_with_range(range, prob, bit, &mut bound);
            if sym >= 0x10000 {
                break;
            }
        }
        self.range = range;
    }

    /// C: `LitEnc_EncodeMatched`.
    pub(crate) fn lit_encode_matched(
        &mut self,
        probs: &mut [u16],
        sym: u32,
        match_byte: u32,
        out: &mut dyn SeqOutStream,
    ) {
        let mut offs = 0x100u32;
        let mut bound = RangeOutput(out);
        let mut range = self.range;
        let mut sym = sym | 0x100;
        let mut match_byte = match_byte;
        loop {
            match_byte <<= 1;
            let i = (offs + (match_byte & offs) + (sym >> 8)) as usize;
            let bit = (sym >> 7) & 1;
            sym <<= 1;
            offs &= !(match_byte ^ sym);
            let prob = &mut probs[i];
            range = self.encode_bit_with_range(range, prob, bit, &mut bound);
            if sym >= 0x10000 {
                break;
            }
        }
        self.range = range;
    }

    /// C: `RcTree_ReverseEncode`.
    pub(crate) fn rc_tree_reverse_encode(
        &mut self,
        probs: &mut [u16],
        num_bits: u32,
        sym: u32,
        out: &mut dyn SeqOutStream,
    ) {
        let mut bound = RangeOutput(out);
        let mut range = self.range;
        let mut m = 1usize;
        let mut sym = sym;
        for _ in 0..num_bits {
            let bit = sym & 1;
            sym >>= 1;
            let prob = &mut probs[m];
            range = self.encode_bit_with_range(range, prob, bit, &mut bound);
            m = (m << 1) | bit as usize;
        }
        self.range = range;
    }
}

#[cfg(test)]
mod tests {
    use super::{RangeEnc, RangeOutput};
    use crate::enc::consts::*;
    use crate::enc::price::LenEnc;
    use crate::enc::stream::SeqOutStream;
    use crate::error::Error;
    use alloc::vec::Vec;

    struct Sink {
        bytes: Vec<u8>,
        calls: usize,
        fail_at: usize,
    }

    impl SeqOutStream for Sink {
        fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
            let call = self.calls;
            self.calls += 1;
            if call == self.fail_at {
                return Err(Error::Write);
            }
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
    }

    // C: the checked, branching RC_BIT definition, with a field writeback
    // after every bit instead of the production symbol-local range.
    fn reference_bit(rc: &mut RangeEnc, prob: &mut u16, bit: u32, out: &mut Sink) {
        let value = u32::from(*prob);
        let bound = (rc.range >> K_NUM_BIT_MODEL_TOTAL_BITS) * value;
        if bit == 0 {
            rc.range = bound;
            *prob = (value + ((K_BIT_MODEL_TOTAL - value) >> K_NUM_MOVE_BITS)) as u16;
        } else {
            rc.range -= bound;
            rc.low += u64::from(bound);
            *prob = (value - (value >> K_NUM_MOVE_BITS)) as u16;
        }
        if rc.range < K_TOP_VALUE {
            rc.range <<= 8;
            rc.shift_low(&mut RangeOutput(out));
        }
    }

    fn reference_tree(
        rc: &mut RangeEnc,
        probs: &mut [u16],
        bits: u32,
        sym: u32,
        reverse: bool,
        out: &mut Sink,
    ) {
        let mut node = 1;
        for i in 0..bits {
            let bit = (sym >> if reverse { i } else { bits - i - 1 }) & 1;
            reference_bit(rc, &mut probs[node], bit, out);
            node = (node << 1) | bit as usize;
        }
    }

    fn reference_length(rc: &mut RangeEnc, len: &mut LenEnc, sym: u32, pos: u32, out: &mut Sink) {
        let mut base = 0;
        let mut sym = sym;
        reference_bit(rc, &mut len.low[0], u32::from(sym >= 8), out);
        if sym >= 8 {
            reference_bit(rc, &mut len.low[8], u32::from(sym >= 16), out);
            if sym >= 16 {
                reference_tree(rc, &mut len.high, 8, sym - 16, false, out);
                return;
            }
            base = 8;
            sym -= 8;
        }
        base += (pos as usize) << 4;
        reference_tree(rc, &mut len.low[base..], 3, sym, false, out);
    }

    #[test]
    fn range_local_length_and_reverse_trees_match_bitwise_reference() {
        for fail_at in [usize::MAX, 0, 1] {
            let mut actual = RangeEnc::new().unwrap();
            let mut expected = RangeEnc::new().unwrap();
            let mut lengths = LenEnc::new();
            let mut expected_lengths = lengths.clone();
            let mut tree = [K_PROB_INIT_VALUE; 64];
            let mut expected_tree = tree;
            let mut out = Sink {
                bytes: Vec::new(),
                calls: 0,
                fail_at,
            };
            let mut reference_out = Sink {
                bytes: Vec::new(),
                calls: 0,
                fail_at,
            };
            for round in 0..16 {
                for pos in 0..16 {
                    for sym in 0..272 {
                        lengths.encode(&mut actual, sym, pos, &mut out);
                        reference_length(
                            &mut expected,
                            &mut expected_lengths,
                            sym,
                            pos,
                            &mut reference_out,
                        );
                        let bits = 1 + (round + pos + sym) % 6;
                        actual.rc_tree_reverse_encode(&mut tree, bits, sym, &mut out);
                        reference_tree(
                            &mut expected,
                            &mut expected_tree,
                            bits,
                            sym,
                            true,
                            &mut reference_out,
                        );
                        assert_eq!(actual.range, expected.range);
                        assert_eq!(actual.low, expected.low);
                        assert_eq!(actual.cache, expected.cache);
                        assert_eq!(actual.cache_size, expected.cache_size);
                        assert_eq!(actual.get_processed(), expected.get_processed());
                        assert_eq!(actual.res.is_err(), expected.res.is_err());
                    }
                }
            }
            actual.flush_data(&mut out);
            actual.flush_stream(&mut out);
            expected.flush_data(&mut reference_out);
            expected.flush_stream(&mut reference_out);
            assert_eq!(lengths.low, expected_lengths.low);
            assert_eq!(lengths.high, expected_lengths.high);
            assert_eq!(tree, expected_tree);
            assert_eq!(out.bytes, reference_out.bytes);
            assert_eq!(out.calls, reference_out.calls);
            assert_eq!(actual.res.is_err(), fail_at < 2);
            assert!(actual.get_processed() > RC_BUF_SIZE as u64);
        }
    }
}
