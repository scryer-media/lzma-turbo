//! LZMA1 decoding.
//!
//! C: `C/LzmaDec.c`, minus the encoder-side helpers.

pub(crate) mod consts;
pub(crate) mod decode;
pub(crate) mod decode_opt;
pub(crate) mod dummy;
pub(crate) mod state;

use crate::error::{Error, FinishMode, Progress, Status};
use consts::*;
use decode_opt::decode_real;
use dummy::{Dummy, lzma_dec_try_dummy};
pub(crate) use state::LzmaDec;
pub use state::LzmaProps;

/// C: `LzmaDec_WriteRem`.
///
/// Copies out the tail of a match that the previous call could not finish
/// because the output limit was reached.
fn lzma_dec_write_rem(p: &mut LzmaDec, limit: usize) {
    let mut len = p.remain_len;
    if len == 0
    /* || len >= kMatchSpecLenStart */
    {
        return;
    }
    {
        let mut dic_pos = p.dic_pos;
        {
            let rem = limit - dic_pos;
            if rem < len as usize {
                len = rem as u32;
                if len == 0 {
                    return;
                }
            }
        }

        if p.check_dic_size == 0 && p.prop.dict_size() - p.processed_pos <= len {
            p.check_dic_size = p.prop.dict_size();
        }

        p.processed_pos = p.processed_pos.wrapping_add(len);
        p.remain_len -= len;
        let dic_buf_size = p.dic_buf_size;
        let rep0 = p.reps[0] as usize;
        let dic: &mut [u8] = &mut p.dic;
        loop {
            dic[dic_pos] = dic[dic_pos.wrapping_sub(rep0).wrapping_add(if dic_pos < rep0 {
                dic_buf_size
            } else {
                0
            })];
            dic_pos += 1;
            len -= 1;
            if len == 0 {
                break;
            }
        }
        p.dic_pos = dic_pos;
    }
}

/// C: `LzmaDec_DecodeReal2`.
///
/// Clamps `limit` so that the fast loop cannot cross the point where
/// `checkDicSize` must change, then fixes `checkDicSize` up afterwards.
///
/// # Safety
///
/// Same contract as [`lzma_dec_decode_real`]: at least
/// `LZMA_REQUIRED_INPUT_MAX` readable bytes past `buf_limit`.
unsafe fn lzma_dec_decode_real2(
    p: &mut LzmaDec,
    mut limit: usize,
    buf_start: *const u8,
    buf_limit: *const u8,
) -> decode::RealResult {
    if p.check_dic_size == 0 {
        let rem = p.prop.dict_size() - p.processed_pos;
        if limit - p.dic_pos > rem as usize {
            limit = p.dic_pos + rem as usize;
        }
    }
    {
        // SAFETY: forwarded verbatim from this function's own contract; `limit`
        // was only lowered, which keeps `p.dic_pos <= limit <= dic_buf_size`.
        let res = unsafe { decode_real(p, limit, buf_start, buf_limit) };
        if p.check_dic_size == 0 && p.processed_pos >= p.prop.dict_size() {
            p.check_dic_size = p.prop.dict_size();
        }
        res
    }
}

/// C: `LzmaDec_DecodeToDic`.
///
/// Decodes into the internal dictionary up to `dic_limit`. Returns the number
/// of input bytes consumed and where it stopped.
#[allow(clippy::too_many_lines)]
// The port keeps the reference decoder's dead stores (the last `NORMALIZE`,
// the last `offs ^= bit`) so the expansion matches the C line for line.
#[allow(unused_assignments)]
pub(crate) fn lzma_dec_decode_to_dic(
    p: &mut LzmaDec,
    dic_limit: usize,
    src: &[u8],
    finish_mode: FinishMode,
    status: &mut Status,
) -> Result<usize, Error> {
    let mut in_size = src.len();
    let mut src_pos = 0usize; // C: the running `src` pointer
    let mut src_len = 0usize; // C: `*srcLen`
    *status = Status::NotSpecified;

    if p.remain_len > K_MATCH_SPEC_LEN_START {
        if p.remain_len > K_MATCH_SPEC_LEN_START + 2 {
            return Err(if p.remain_len == K_MATCH_SPEC_LEN_ERROR_FAIL {
                Error::InternalFailure
            } else {
                Error::CorruptData
            });
        }

        while in_size > 0 && p.temp_buf_size < RC_INIT_SIZE {
            p.temp_buf[p.temp_buf_size] = src[src_pos];
            p.temp_buf_size += 1;
            src_pos += 1;
            src_len += 1;
            in_size -= 1;
        }
        if p.temp_buf_size != 0 && p.temp_buf[0] != 0 {
            return Err(Error::CorruptData);
        }
        if p.temp_buf_size < RC_INIT_SIZE {
            *status = Status::NeedsMoreInput;
            return Ok(src_len);
        }
        p.code = (u32::from(p.temp_buf[1]) << 24)
            | (u32::from(p.temp_buf[2]) << 16)
            | (u32::from(p.temp_buf[3]) << 8)
            | u32::from(p.temp_buf[4]);

        if p.check_dic_size == 0 && p.processed_pos == 0 && p.code >= K_BAD_REP_CODE {
            return Err(Error::CorruptData);
        }

        p.range = 0xFFFF_FFFF;
        p.temp_buf_size = 0;

        if p.remain_len > K_MATCH_SPEC_LEN_START + 1 {
            p.reset_state();
        }

        p.remain_len = 0;
    }

    loop {
        if p.remain_len == K_MATCH_SPEC_LEN_START {
            if p.code != 0 {
                return Err(Error::CorruptData);
            }
            *status = Status::FinishedWithMark;
            return Ok(src_len);
        }

        lzma_dec_write_rem(p, dic_limit);

        {
            // (p->remainLen == 0 || p->dicPos == dicLimit)

            let mut check_end_mark_now = false;

            if p.dic_pos >= dic_limit {
                if p.remain_len == 0 && p.code == 0 {
                    *status = Status::MaybeFinishedWithoutMark;
                    return Ok(src_len);
                }
                if finish_mode == FinishMode::Any {
                    *status = Status::NotFinished;
                    return Ok(src_len);
                }
                if p.remain_len != 0 {
                    // C: RETURN_NOT_FINISHED_FOR_FINISH (strict mode)
                    *status = Status::NotFinished;
                    return Err(Error::CorruptData);
                }
                check_end_mark_now = true;
            }

            // (p->remainLen == 0)

            if p.temp_buf_size == 0 {
                let buf_limit_off: usize;
                let mut dummy_processed: isize = -1;

                if in_size < LZMA_REQUIRED_INPUT_MAX || check_end_mark_now {
                    let (dummy_res, dummy_len) =
                        lzma_dec_try_dummy(p, &src[src_pos..src_pos + in_size]);

                    if dummy_res == Dummy::InputEof {
                        if in_size >= LZMA_REQUIRED_INPUT_MAX {
                            break;
                        }
                        src_len += in_size;
                        p.temp_buf_size = in_size;
                        p.temp_buf[..in_size].copy_from_slice(&src[src_pos..src_pos + in_size]);
                        *status = Status::NeedsMoreInput;
                        return Ok(src_len);
                    }

                    dummy_processed = dummy_len as isize;
                    if dummy_len > LZMA_REQUIRED_INPUT_MAX {
                        break;
                    }

                    if check_end_mark_now && !dummy_res.end_marker_possible() {
                        src_len += dummy_len;
                        p.temp_buf_size = dummy_len;
                        p.temp_buf[..dummy_len].copy_from_slice(&src[src_pos..src_pos + dummy_len]);
                        *status = Status::NotFinished;
                        return Err(Error::CorruptData);
                    }

                    buf_limit_off = 0;
                    // we will decode only one iteration
                } else {
                    buf_limit_off = in_size - LZMA_REQUIRED_INPUT_MAX;
                }

                {
                    let base = src.as_ptr();
                    // SAFETY: `src_pos <= src.len()` and
                    // `src_pos + buf_limit_off <= src_pos + in_size <= src.len()`,
                    // so both pointers are inside the same allocation. The
                    // margin the fast loop needs is exactly what
                    // `buf_limit_off` leaves: either `in_size - 20` bytes, or
                    // 0 with a single symbol known (from `try_dummy`) to need
                    // at most `in_size` bytes.
                    let (buf_start, buf_limit) =
                        unsafe { (base.add(src_pos), base.add(src_pos + buf_limit_off)) };
                    // SAFETY: see above; `dic_limit <= p.dic_buf_size` is the
                    // caller's contract and `p.dic_pos <= dic_limit` holds
                    // because the `p.dic_pos >= dic_limit` case returned above.
                    let res = unsafe { lzma_dec_decode_real2(p, dic_limit, buf_start, buf_limit) };

                    let processed = (res.buf as usize) - (buf_start as usize);

                    if dummy_processed < 0 {
                        if processed > in_size {
                            break;
                        }
                    } else if dummy_processed as usize != processed {
                        break;
                    }

                    src_pos += processed;
                    in_size -= processed;
                    src_len += processed;

                    if !res.ok {
                        p.remain_len = K_MATCH_SPEC_LEN_ERROR_DATA;
                        return Err(Error::CorruptData);
                    }
                }
                continue;
            }

            {
                // we have some data in (p->tempBuf)
                // in strict mode: tempBufSize is not enough for one Symbol decoding.
                // in relaxed mode: tempBufSize not larger than required for one Symbol decoding.

                let mut rem = p.temp_buf_size;
                let mut ahead = 0usize;
                let mut dummy_processed: isize = -1;

                while rem < LZMA_REQUIRED_INPUT_MAX && ahead < in_size {
                    p.temp_buf[rem] = src[src_pos + ahead];
                    rem += 1;
                    ahead += 1;
                }

                // ahead - the size of new data copied from (src) to (p->tempBuf)
                // rem   - the size of temp buffer including new data from (src)

                if rem < LZMA_REQUIRED_INPUT_MAX || check_end_mark_now {
                    let temp = p.temp_buf;
                    let (dummy_res, dummy_len) = lzma_dec_try_dummy(p, &temp[..rem]);

                    if dummy_res == Dummy::InputEof {
                        if rem >= LZMA_REQUIRED_INPUT_MAX {
                            break;
                        }
                        p.temp_buf_size = rem;
                        src_len += ahead;
                        *status = Status::NeedsMoreInput;
                        return Ok(src_len);
                    }

                    dummy_processed = dummy_len as isize;

                    if dummy_len < p.temp_buf_size {
                        break;
                    }

                    if check_end_mark_now && !dummy_res.end_marker_possible() {
                        src_len += dummy_len - p.temp_buf_size;
                        p.temp_buf_size = dummy_len;
                        *status = Status::NotFinished;
                        return Err(Error::CorruptData);
                    }
                }

                {
                    // we decode one symbol from (p->tempBuf) here, so the (bufLimit) is equal to (p->buf)
                    // A copy, not a pointer into `p`: passing `p` below as
                    // `&mut` would invalidate a pointer into `p.temp_buf`.
                    let temp = p.temp_buf;
                    let base = temp.as_ptr();
                    // SAFETY: `temp` is `LZMA_REQUIRED_INPUT_MAX` bytes and
                    // `buf_limit == buf_start`, so the loop runs exactly one
                    // iteration, which consumes at most
                    // `LZMA_REQUIRED_INPUT_MAX` bytes: the whole buffer is the
                    // margin.
                    let res = unsafe { lzma_dec_decode_real2(p, dic_limit, base, base) };

                    let processed = (res.buf as usize) - (base as usize);
                    rem = p.temp_buf_size;

                    if dummy_processed < 0 {
                        if processed > LZMA_REQUIRED_INPUT_MAX {
                            break;
                        }
                        if processed < rem {
                            break;
                        }
                    } else if dummy_processed as usize != processed {
                        break;
                    }

                    let processed = processed - rem;

                    src_pos += processed;
                    in_size -= processed;
                    src_len += processed;
                    p.temp_buf_size = 0;

                    if !res.ok {
                        p.remain_len = K_MATCH_SPEC_LEN_ERROR_DATA;
                        return Err(Error::CorruptData);
                    }
                }
            }
        }
    }

    /*  Some unexpected error: internal error of code, memory corruption or hardware failure */
    p.remain_len = K_MATCH_SPEC_LEN_ERROR_FAIL;
    Err(Error::InternalFailure)
}

/// LZMA1 decoder.
///
/// Wraps the reference `CLzmaDec` state and its buffer-level entry point.
pub struct LzmaDecoder {
    p: LzmaDec,
}

impl LzmaDecoder {
    /// Allocates a decoder for `props`.
    ///
    /// C: `LzmaDec_Allocate` followed by `LzmaDec_Init`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Alloc`] if the dictionary or probability table cannot
    /// be allocated.
    pub fn new(props: LzmaProps) -> Result<Self, Error> {
        Ok(LzmaDecoder {
            p: LzmaDec::new(props)?,
        })
    }

    /// As [`LzmaDecoder::new`], with the dictionary on caller memory of
    /// exactly `props.dic_buf_size()` bytes.
    #[cfg(feature = "std")]
    pub(crate) fn with_caller_dic(
        props: LzmaProps,
        dic: crate::window::CallerWindow,
    ) -> Result<Self, Error> {
        Ok(LzmaDecoder {
            p: LzmaDec::with_caller_dic(props, dic)?,
        })
    }

    /// Same as [`LzmaDecoder::new`], but always runs the portable decode loop
    /// even where an assembly one is compiled in.
    ///
    /// This exists so the two loops can be run against each other; there is no
    /// reason to prefer it otherwise, and it is not part of the supported API.
    ///
    /// # Errors
    ///
    /// As [`LzmaDecoder::new`].
    #[doc(hidden)]
    pub fn new_portable(props: LzmaProps) -> Result<Self, Error> {
        let mut d = Self::new(props)?;
        d.p.force_portable = true;
        Ok(d)
    }

    /// The properties this decoder was built for.
    #[must_use]
    pub fn props(&self) -> LzmaProps {
        self.p.prop
    }

    /// C: `LzmaDec_Init`. Restarts the decoder for a new stream with the same
    /// properties.
    pub fn reset(&mut self) {
        self.p.init();
    }

    /// C: `LzmaDec_DecodeToBuf`.
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
        let mut in_size = input.len();
        let mut src_pos = 0usize;
        let mut dest_pos = 0usize;
        let mut status = Status::NotSpecified;

        loop {
            if self.p.dic_pos == self.p.dic_buf_size {
                self.p.dic_pos = 0;
            }
            let dic_pos = self.p.dic_pos;
            let (out_size_cur, cur_finish_mode) = if out_size > self.p.dic_buf_size - dic_pos {
                (self.p.dic_buf_size, FinishMode::Any)
            } else {
                (dic_pos + out_size, finish)
            };

            let res = lzma_dec_decode_to_dic(
                &mut self.p,
                out_size_cur,
                &input[src_pos..],
                cur_finish_mode,
                &mut status,
            );

            let in_size_cur = match res {
                Ok(n) => n,
                Err(e) => {
                    // The C copies out whatever the failing call produced
                    // before returning the error; the counts are meaningless
                    // to a caller that must stop anyway.
                    return Err(e);
                }
            };

            src_pos += in_size_cur;
            in_size -= in_size_cur;

            let out_size_cur = self.p.dic_pos - dic_pos;
            output[dest_pos..dest_pos + out_size_cur]
                .copy_from_slice(&self.p.dic[dic_pos..dic_pos + out_size_cur]);
            dest_pos += out_size_cur;
            out_size -= out_size_cur;

            if out_size_cur == 0 || out_size == 0 {
                let _ = in_size;
                return Ok(Progress {
                    read: src_pos,
                    written: dest_pos,
                    status,
                });
            }
        }
    }
}
