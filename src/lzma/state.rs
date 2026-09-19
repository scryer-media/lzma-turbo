//! Decoder state and properties.
//!
//! C: `CLzmaProps` and `CLzmaDec` in `C/LzmaDec.h`, plus `LzmaProps_Decode`,
//! `LzmaDec_AllocateProbs2` and `LzmaDec_Allocate` from `C/LzmaDec.c`.

use alloc::vec::Vec;

use crate::error::Error;
use crate::lzma::consts::*;

/// Parsed LZMA properties: the literal-context, literal-position and position
/// bit counts, and the dictionary size.
///
/// C: `CLzmaProps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LzmaProps {
    lc: u8,
    lp: u8,
    pb: u8,
    dict_size: u32,
}

impl LzmaProps {
    /// C: `LzmaProps_Decode`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProps`] if the first byte encodes an
    /// `lc`/`lp`/`pb` triple outside the format's range.
    pub fn parse(props: &[u8; LZMA_PROPS_SIZE]) -> Result<Self, Error> {
        let mut dict_size = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);

        if dict_size < LZMA_DIC_MIN {
            dict_size = LZMA_DIC_MIN;
        }

        let mut d = props[0];
        if d >= (9 * 5 * 5) {
            return Err(Error::UnsupportedProps);
        }

        let lc = d % 9;
        d /= 9;
        let pb = d / 5;
        let lp = d % 5;

        Ok(LzmaProps {
            lc,
            lp,
            pb,
            dict_size,
        })
    }

    /// Builds properties directly. Used by the LZMA2 decoder, which derives
    /// them from its single property byte rather than a 5-byte block.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProps`] if any field is out of range.
    pub fn new(lc: u8, lp: u8, pb: u8, dict_size: u32) -> Result<Self, Error> {
        if lc > 8 || lp > 4 || pb > 4 {
            return Err(Error::UnsupportedProps);
        }
        Ok(LzmaProps {
            lc,
            lp,
            pb,
            dict_size: dict_size.max(LZMA_DIC_MIN),
        })
    }

    /// Literal context bits.
    #[must_use]
    pub const fn lc(&self) -> u8 {
        self.lc
    }
    /// Literal position bits.
    #[must_use]
    pub const fn lp(&self) -> u8 {
        self.lp
    }
    /// Position bits.
    #[must_use]
    pub const fn pb(&self) -> u8 {
        self.pb
    }
    /// Dictionary size in bytes, as recorded in the stream header.
    #[must_use]
    pub const fn dict_size(&self) -> u32 {
        self.dict_size
    }

    /// C: the `LZMA2_STATE_PROP` case of `Lzma2Dec_UpdateState`, which writes
    /// `lc`/`lp`/`pb` straight into `p->decoder.prop`.
    pub(crate) fn set_lclppb(&mut self, lc: u8, lp: u8, pb: u8) {
        self.lc = lc;
        self.lp = lp;
        self.pb = pb;
    }

    /// C: the `dicBufSize` rounding in `LzmaDec_Allocate`.
    pub(crate) fn dic_buf_size(&self) -> usize {
        let dict_size = self.dict_size;
        let mask: usize = if dict_size >= (1 << 30) {
            (1 << 22) - 1
        } else if dict_size >= (1 << 22) {
            (1 << 20) - 1
        } else {
            (1 << 12) - 1
        };
        let dic_buf_size = (dict_size as usize).wrapping_add(mask) & !mask;
        if dic_buf_size < dict_size as usize {
            dict_size as usize
        } else {
            dic_buf_size
        }
    }
}

/// C: `CLzmaDec`.
///
/// `p->buf` is absent: the reference keeps the current input pointer in the
/// struct only so the fast loop can hand it back, and the port passes it in
/// and out of [`crate::lzma::decode::lzma_dec_decode_real`] instead.
pub(crate) struct LzmaDec {
    pub(crate) prop: LzmaProps,
    pub(crate) probs: Vec<u16>,
    pub(crate) dic: Vec<u8>,
    pub(crate) dic_buf_size: usize,
    pub(crate) dic_pos: usize,
    pub(crate) range: u32,
    pub(crate) code: u32,
    pub(crate) processed_pos: u32,
    pub(crate) check_dic_size: u32,
    pub(crate) reps: [u32; 4],
    pub(crate) state: u32,
    pub(crate) remain_len: u32,
    pub(crate) temp_buf_size: usize,
    pub(crate) temp_buf: [u8; LZMA_REQUIRED_INPUT_MAX],
    /// Forces the portable decode loop even on a target that has an assembly
    /// one. Only [`crate::LzmaDecoder::new_portable`] sets it, so that the two
    /// loops can be run against each other in tests.
    pub(crate) force_portable: bool,
}

impl LzmaDec {
    /// C: `LzmaDec_Allocate` (which is `LzmaProps_Decode` +
    /// `LzmaDec_AllocateProbs2` + the dictionary allocation).
    pub(crate) fn new(prop: LzmaProps) -> Result<Self, Error> {
        let dic_buf_size = prop.dic_buf_size();
        Self::alloc(prop, dic_buf_size)
    }

    /// C: `LzmaDec_AllocateProbs`, which allocates the probability table and
    /// records the properties but leaves `dic` null. The multi-threaded
    /// decoder uses it because each worker's dictionary *is* its output block,
    /// handed to it per block rather than owned by the decoder.
    #[cfg(feature = "std")]
    pub(crate) fn new_probs_only(prop: LzmaProps) -> Result<Self, Error> {
        Self::alloc(prop, 0)
    }

    fn alloc(prop: LzmaProps, dic_buf_size: usize) -> Result<Self, Error> {
        let num_probs = lzma_props_get_num_probs(prop.lc, prop.lp);

        let mut probs = Vec::new();
        probs
            .try_reserve_exact(num_probs)
            .map_err(|_| Error::Alloc)?;
        probs.resize(num_probs, 0);

        let mut dic = Vec::new();
        dic.try_reserve_exact(dic_buf_size)
            .map_err(|_| Error::Alloc)?;
        dic.resize(dic_buf_size, 0);

        let mut p = LzmaDec {
            prop,
            probs,
            dic,
            dic_buf_size,
            dic_pos: 0,
            range: 0,
            code: 0,
            processed_pos: 0,
            check_dic_size: 0,
            reps: [0; 4],
            state: 0,
            remain_len: 0,
            temp_buf_size: 0,
            temp_buf: [0; LZMA_REQUIRED_INPUT_MAX],
            force_portable: false,
        };
        p.init();
        Ok(p)
    }

    /// C: `LzmaDec_Init`.
    pub(crate) fn init(&mut self) {
        self.dic_pos = 0;
        self.init_dic_and_state(true, true);
    }

    /// C: `LzmaDec_InitDicAndState`.
    pub(crate) fn init_dic_and_state(&mut self, init_dic: bool, init_state: bool) {
        self.remain_len = K_MATCH_SPEC_LEN_START + 1;
        self.temp_buf_size = 0;

        if init_dic {
            self.processed_pos = 0;
            self.check_dic_size = 0;
            self.remain_len = K_MATCH_SPEC_LEN_START + 2;
        }
        if init_state {
            self.remain_len = K_MATCH_SPEC_LEN_START + 2;
        }
    }

    /// C: the `for (i = 0; i < numProbs; i++) probs[i] = kBitModelTotal >> 1;`
    /// block plus the rep/state reset in `LzmaDec_DecodeToDic`.
    pub(crate) fn reset_state(&mut self) {
        let num_probs = lzma_props_get_num_probs(self.prop.lc, self.prop.lp);
        debug_assert!(num_probs <= self.probs.len());
        self.probs[..num_probs].fill((K_BIT_MODEL_TOTAL >> 1) as u16);
        self.reps = [1, 1, 1, 1];
        self.state = 0;
    }
}
