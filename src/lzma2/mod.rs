//! LZMA2 decoding: chunk framing over the LZMA1 decoder.
//!
//! C: `C/Lzma2Dec.c`. The chunk-header state machine itself lives in
//! [`frame`], because [`parse`] runs the same one without a dictionary for
//! the multi-threaded decoder's benefit.
//!
//! ```text
//! 00000000  -  End of data
//! 00000001 U U  -  Uncompressed, reset dic, need reset state and set new prop
//! 00000010 U U  -  Uncompressed, no reset
//! 100uuuuu U U P P  -  LZMA, no reset
//! 101uuuuu U U P P  -  LZMA, reset state
//! 110uuuuu U U P P S  -  LZMA, reset state + set new prop
//! 111uuuuu U U P P S  -  LZMA, reset state + set new prop, reset dic
//!
//!   u, U - Unpack Size
//!   P - Pack Size
//!   S - Props
//! ```

pub(crate) mod frame;
// The chunk-header walker exists for the multi-threaded decoder, which is
// itself behind `std`; without it nothing in the crate parses without
// decoding.
#[cfg(feature = "std")]
pub(crate) mod parse;
pub mod scan;

use crate::error::{Error, FinishMode, Progress, Status};
use crate::lzma::consts::LZMA_DIC_MIN;
use crate::lzma::{LzmaDec, LzmaProps, lzma_dec_decode_to_dic};

use frame::{
    LZMA2_CONTROL_COPY_RESET_DIC, LZMA2_LCLP_MAX, Lzma2Frame, Lzma2State, is_uncompressed_state,
};

/// LZMA2 decoder.
///
/// C: `CLzma2Dec`.
pub struct Lzma2Decoder {
    frame: Lzma2Frame,
    pub(crate) decoder: LzmaDec,
}

impl Lzma2Decoder {
    /// Allocates a decoder for the LZMA2 dictionary-size property byte, the
    /// single byte an xz filter flag or a 7z coder carries.
    ///
    /// C: `Lzma2Dec_Allocate` + `Lzma2Dec_Init`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProps`] if `dict_prop > 40`, and
    /// [`Error::Alloc`] if the dictionary cannot be allocated.
    pub fn new(dict_prop: u8) -> Result<Self, Error> {
        // C: Lzma2Dec_GetOldProps
        if dict_prop > 40 {
            return Err(Error::UnsupportedProps);
        }
        let dic_size = frame::dic_size_from_prop_full(dict_prop);
        // C: props[0] = LZMA2_LCLP_MAX, i.e. lc = 4, lp = 0, pb = 0, which
        // sizes the probability table for the largest (lc + lp) LZMA2 allows.
        let prop = LzmaProps::new(LZMA2_LCLP_MAX, 0, 0, dic_size.max(LZMA_DIC_MIN))?;
        let mut p = Lzma2Decoder {
            frame: Lzma2Frame::new(),
            decoder: LzmaDec::new(prop)?,
        };
        p.reset();
        Ok(p)
    }

    /// Allocates a decoder whose dictionary is clamped to `cap` bytes,
    /// with the format's minimum allocation of 4096 bytes and the SDK's
    /// dictionary allocation rounding. The usable match distance is capped;
    /// the backing allocation may be rounded up.
    ///
    /// C: nothing - 7-Zip always allocates the declared dictionary. The xz
    /// container can do better: it resets the dictionary at every block, so no
    /// match in a block can reach further back than that block's first byte,
    /// and when the block header declares an uncompressed size the dictionary
    /// never has to be larger than it. That turns an `xz -9` stream of small
    /// blocks from 64 MiB per worker into a few kilobytes.
    ///
    /// This also applies to independently reset LZMA2 runs. The caller is
    /// responsible for `cap` being at least as large as the block or run's
    /// output; a smaller `cap` decodes a block whose matches reach
    /// past it into [`Error::CorruptData`], not into wrong bytes, because the
    /// distance check is against the dictionary size.
    ///
    /// # Errors
    ///
    /// As [`Lzma2Decoder::new`].
    pub fn new_capped(dict_prop: u8, cap: u32) -> Result<Self, Error> {
        if dict_prop > 40 {
            return Err(Error::UnsupportedProps);
        }
        let dic_size = frame::dic_size_from_prop_full(dict_prop).min(cap);
        let prop = LzmaProps::new(LZMA2_LCLP_MAX, 0, 0, dic_size.max(LZMA_DIC_MIN))?;
        let mut p = Lzma2Decoder {
            frame: Lzma2Frame::new(),
            decoder: LzmaDec::new(prop)?,
        };
        p.reset();
        Ok(p)
    }

    /// Same as [`Lzma2Decoder::new`], but always runs the portable decode
    /// loop. See [`crate::LzmaDecoder::new_portable`].
    ///
    /// # Errors
    ///
    /// As [`Lzma2Decoder::new`].
    #[doc(hidden)]
    pub fn new_portable(dict_prop: u8) -> Result<Self, Error> {
        let mut d = Self::new(dict_prop)?;
        d.decoder.force_portable = true;
        Ok(d)
    }

    /// C: `Lzma2Dec_Init`.
    pub fn reset(&mut self) {
        self.frame.init();
        self.decoder.init();
    }

    /// C: `Lzma2Dec_UpdateState`.
    fn update_state(&mut self, b: u8) -> Lzma2State {
        self.frame.update_state(b, &mut self.decoder.prop)
    }

    /// C: `Lzma2Dec_DecodeToDic`.
    #[allow(clippy::too_many_lines)]
    fn decode_to_dic(
        &mut self,
        dic_limit: usize,
        src: &[u8],
        finish_mode: FinishMode,
        status: &mut Status,
    ) -> Result<usize, Error> {
        let in_size = src.len();
        let mut src_len = 0usize;
        *status = Status::NotSpecified;

        while self.frame.state != Lzma2State::Error {
            if self.frame.state == Lzma2State::Finished {
                *status = Status::FinishedWithMark;
                return Ok(src_len);
            }

            let dic_pos = self.decoder.dic_pos;

            if dic_pos == dic_limit && finish_mode == FinishMode::Any {
                *status = Status::NotFinished;
                return Ok(src_len);
            }

            if self.frame.state != Lzma2State::Data && self.frame.state != Lzma2State::DataCont {
                if src_len == in_size {
                    *status = Status::NeedsMoreInput;
                    return Ok(src_len);
                }
                let b = src[src_len];
                src_len += 1;
                self.frame.state = self.update_state(b);
                if dic_pos == dic_limit && self.frame.state != Lzma2State::Finished {
                    break;
                }
                continue;
            }

            {
                let mut in_cur = in_size - src_len;
                let mut out_cur = dic_limit - dic_pos;
                let mut cur_finish_mode = FinishMode::Any;

                if out_cur >= self.frame.unpack_size as usize {
                    out_cur = self.frame.unpack_size as usize;
                    cur_finish_mode = FinishMode::End;
                }

                if is_uncompressed_state(self.frame.control) {
                    if in_cur == 0 {
                        *status = Status::NeedsMoreInput;
                        return Ok(src_len);
                    }

                    if self.frame.state == Lzma2State::Data {
                        let init_dic = self.frame.control == LZMA2_CONTROL_COPY_RESET_DIC;
                        self.decoder.init_dic_and_state(init_dic, false);
                    }

                    if in_cur > out_cur {
                        in_cur = out_cur;
                    }
                    if in_cur == 0 {
                        break;
                    }

                    // C: LzmaDec_UpdateWithUncompressed
                    let p = &mut self.decoder;
                    p.dic[p.dic_pos..p.dic_pos + in_cur]
                        .copy_from_slice(&src[src_len..src_len + in_cur]);
                    p.dic_pos += in_cur;
                    if p.check_dic_size == 0
                        && p.prop.dict_size() - p.processed_pos <= in_cur as u32
                    {
                        p.check_dic_size = p.prop.dict_size();
                    }
                    p.processed_pos = p.processed_pos.wrapping_add(in_cur as u32);

                    src_len += in_cur;
                    self.frame.unpack_size -= in_cur as u32;
                    self.frame.state = if self.frame.unpack_size == 0 {
                        Lzma2State::Control
                    } else {
                        Lzma2State::DataCont
                    };
                } else {
                    if self.frame.state == Lzma2State::Data {
                        let init_dic = self.frame.control >= 0xE0;
                        let init_state = self.frame.control >= 0xA0;
                        self.decoder.init_dic_and_state(init_dic, init_state);
                        self.frame.state = Lzma2State::DataCont;
                    }

                    if in_cur > self.frame.pack_size as usize {
                        in_cur = self.frame.pack_size as usize;
                    }

                    let res = lzma_dec_decode_to_dic(
                        &mut self.decoder,
                        dic_pos + out_cur,
                        &src[src_len..src_len + in_cur],
                        cur_finish_mode,
                        status,
                    );

                    let in_cur = res?;

                    src_len += in_cur;
                    self.frame.pack_size -= in_cur as u32;
                    let out_cur = self.decoder.dic_pos - dic_pos;
                    self.frame.unpack_size -= out_cur as u32;

                    if *status == Status::NeedsMoreInput {
                        if self.frame.pack_size == 0 {
                            break;
                        }
                        return Ok(src_len);
                    }

                    if in_cur == 0 && out_cur == 0 {
                        if *status != Status::MaybeFinishedWithoutMark
                            || self.frame.unpack_size != 0
                            || self.frame.pack_size != 0
                        {
                            break;
                        }
                        self.frame.state = Lzma2State::Control;
                    }

                    *status = Status::NotSpecified;
                }
            }
        }

        *status = Status::NotSpecified;
        self.frame.state = Lzma2State::Error;
        Err(Error::CorruptData)
    }

    /// C: `Lzma2Dec_DecodeToBuf`.
    ///
    /// Consumes bytes from `input`, writes decoded bytes to `output`, and
    /// reports where it stopped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CorruptData`] for malformed input.
    pub fn decode(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        finish: FinishMode,
    ) -> Result<Progress, Error> {
        let mut out_size = output.len();
        let mut src_pos = 0usize;
        let mut dest_pos = 0usize;
        let mut status = Status::NotSpecified;

        loop {
            if self.decoder.dic_pos == self.decoder.dic_buf_size {
                self.decoder.dic_pos = 0;
            }
            let dic_pos = self.decoder.dic_pos;
            let mut cur_finish_mode = FinishMode::Any;
            let mut out_cur = self.decoder.dic_buf_size - dic_pos;

            if out_cur >= out_size {
                out_cur = out_size;
                cur_finish_mode = finish;
            }

            let in_cur = self.decode_to_dic(
                dic_pos + out_cur,
                &input[src_pos..],
                cur_finish_mode,
                &mut status,
            )?;

            src_pos += in_cur;
            let out_cur = self.decoder.dic_pos - dic_pos;
            output[dest_pos..dest_pos + out_cur]
                .copy_from_slice(&self.decoder.dic[dic_pos..dic_pos + out_cur]);
            dest_pos += out_cur;
            out_size -= out_cur;

            if out_cur == 0 || out_size == 0 {
                return Ok(Progress {
                    read: src_pos,
                    written: dest_pos,
                    status,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Hooks for the multi-threaded decoder.
//
// C: `Lzma2DecMt_MtCallback_PreCode` points `t->dec.decoder.dic` straight at
// the thread's output block and sets `dicBufSize` to the block's size, so the
// worker decodes into the buffer that is later written out, with no copy and
// with no dictionary of its own. `Lzma2Dec_AllocateProbs` is what it calls
// instead of `Lzma2Dec_Allocate`, for the same reason.
// ---------------------------------------------------------------------------

impl Lzma2Decoder {
    /// C: `Lzma2Dec_AllocateProbs`. Builds a decoder with the probability
    /// table but no dictionary of its own; the caller supplies one per block
    /// with [`Lzma2Decoder::set_block_dic`].
    #[cfg(feature = "std")]
    pub(crate) fn new_probs_only(dict_prop: u8) -> Result<Self, Error> {
        if dict_prop > 40 {
            return Err(Error::UnsupportedProps);
        }
        let dic_size = frame::dic_size_from_prop_full(dict_prop);
        let prop = LzmaProps::new(LZMA2_LCLP_MAX, 0, 0, dic_size.max(LZMA_DIC_MIN))?;
        let mut p = Lzma2Decoder {
            frame: Lzma2Frame::new(),
            decoder: LzmaDec::new_probs_only(prop)?,
        };
        p.reset();
        Ok(p)
    }

    /// Installs `buf` as this decoder's dictionary for one block and declares
    /// the block `size` bytes long, then re-initialises the decoder.
    ///
    /// `buf.len()` may exceed `size`: the buffer is reused across blocks and
    /// only ever grows, and the decoder reads and writes strictly below
    /// `dic_buf_size`.
    ///
    /// C: the three assignments at the end of `Lzma2DecMt_MtCallback_PreCode`
    /// plus the `Lzma2Dec_Init` that `..._Code` does on `needInit`.
    #[cfg(feature = "std")]
    pub(crate) fn set_block_dic(&mut self, buf: alloc::vec::Vec<u8>, size: usize) {
        debug_assert!(buf.len() >= size);
        self.decoder.dic = buf;
        self.decoder.dic_buf_size = size;
        self.reset();
    }

    /// Hands the block buffer back, so the caller can write it out and then
    /// return it for the next block.
    #[cfg(feature = "std")]
    pub(crate) fn take_block_dic(&mut self) -> alloc::vec::Vec<u8> {
        self.decoder.dic_buf_size = 0;
        self.decoder.dic_pos = 0;
        core::mem::take(&mut self.decoder.dic)
    }

    /// C: `p->decoder.dicPos`. How much of the current block has been decoded.
    #[cfg(feature = "std")]
    pub(crate) fn dic_pos(&self) -> usize {
        self.decoder.dic_pos
    }

    /// C: `Lzma2Dec_DecodeToDic`, exposed for the multi-threaded decoder,
    /// which decodes a whole block into its own dictionary and never needs the
    /// copy-out step of [`Lzma2Decoder::decode`].
    #[cfg(feature = "std")]
    pub(crate) fn decode_block(
        &mut self,
        dic_limit: usize,
        src: &[u8],
        finish_mode: FinishMode,
    ) -> Result<(usize, Status), Error> {
        let mut status = Status::NotSpecified;
        let read = self.decode_to_dic(dic_limit, src, finish_mode, &mut status)?;
        Ok((read, status))
    }
}

/// Accessors the single-threaded tail of the multi-threaded decoder needs, so
/// that it can stream out of the dictionary the way `Lzma2Dec_Decode_ST` does
/// instead of copying through an intermediate buffer.
#[cfg(feature = "std")]
impl Lzma2Decoder {
    /// C: `p->decoder.dicBufSize`.
    pub(crate) fn dic_buf_size(&self) -> usize {
        self.decoder.dic_buf_size
    }

    /// C: the `if (dec->decoder.dicPos == dec->decoder.dicBufSize) dicPos = 0`
    /// wrap of `Lzma2Dec_Decode_ST`.
    pub(crate) fn wrap_dic_pos(&mut self) {
        if self.decoder.dic_pos == self.decoder.dic_buf_size {
            self.decoder.dic_pos = 0;
        }
    }

    /// Whether the decoder sits between chunks: no chunk half-decoded, none
    /// owing output or input. A stream may only end here.
    ///
    /// C: the `p->state == LZMA2_STATE_CONTROL` the `Lzma2Dec_Decode_ST` loop
    /// tests before it accepts an end marker.
    pub(crate) fn at_chunk_boundary(&self) -> bool {
        self.frame.state == crate::lzma2::frame::Lzma2State::Control
            && self.frame.unpack_size == 0
            && self.frame.pack_size == 0
    }

    /// A stored payload beginning in this input, after any complete header.
    /// The normal decoder still validates the header and updates history;
    /// run delivery can then write the source slice without reading the ring
    /// back. Incomplete headers use the ordinary dictionary delivery path.
    pub(crate) fn stored_payload(&self, input: &[u8]) -> Option<(usize, usize)> {
        if matches!(self.frame.state, Lzma2State::Data | Lzma2State::DataCont)
            && is_uncompressed_state(self.frame.control)
        {
            Some((0, self.frame.unpack_size as usize))
        } else if self.at_chunk_boundary() && input.len() >= 3 && matches!(input[0], 1 | 2) {
            Some((3, usize::from(u16::from_be_bytes([input[1], input[2]])) + 1))
        } else {
            None
        }
    }

    /// The decoded bytes between two dictionary positions.
    pub(crate) fn dic_slice(&self, from: usize, to: usize) -> &[u8] {
        &self.decoder.dic[from..to]
    }
}
