//! The LZMA2 half of the multi-threaded decoder: the four callbacks that
//! [`crate::mt::mtdec::MtDec`] drives.
//!
//! C: the `Lzma2DecMt_MtCallback_*` functions and `CLzma2DecMtThread` in
//! `C/Lzma2DecMt.c`.

use alloc::vec::Vec;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, FinishMode};
use crate::lzma2::Lzma2Decoder;
use crate::lzma2::parse::{Lzma2Parser, ParseStatus};
use crate::mt::mtdec::{CallbackInfo, Coder, MtError, ParseState, WriteCtx};

#[cfg(feature = "crc")]
use crate::mt::ChecksumSink;
#[cfg(feature = "crc")]
use crate::mt::checksum::{BlockChecks, ChecksumPlan, Segmenter};

/// C: the `(1 << 14)` in `Lzma2DecMt_MtCallback_Parse`. Blocks smaller than
/// this are coalesced into their successor rather than given a thread each:
/// "we decode small blocks in one thread".
const SMALL_BLOCK: usize = 1 << 14;

/// The parameters every worker needs, copied into each one.
#[derive(Debug, Clone)]
pub(crate) struct Lzma2CoderProps {
    /// The LZMA2 dictionary-size property byte.
    pub(crate) prop: u8,
    /// C: `props.outBlockMax`.
    pub(crate) out_block_max: usize,
    /// C: `p->outSize` / `p->outSize_Defined`.
    pub(crate) out_size: Option<u64>,
    /// C: `p->finishMode`.
    pub(crate) finish_mode: bool,
    /// Set by whichever worker writes the block that ends the stream.
    ///
    /// C: nothing. `Lzma2DecMt_Decode` returns `SZ_OK` whether or not the
    /// threaded pass reached the end marker, so a stream that stops early
    /// decodes to a prefix and reports success. The caller checks this
    /// instead.
    pub(crate) finished: Arc<AtomicBool>,
    /// What each worker is to checksum over the block it produced, and where
    /// to put the answer.
    ///
    /// C: nothing; see [`crate::checksum`] for why this belongs to the
    /// worker and not to whoever consumes the output.
    #[cfg(feature = "crc")]
    pub(crate) plan: ChecksumPlan,
    #[cfg(feature = "crc")]
    pub(crate) checks: Option<ChecksumSink>,
}

/// C: `CLzma2DecMtThread`.
pub(crate) struct Lzma2Coder {
    props: Lzma2CoderProps,

    /// C: `t->dec`, used for coding. The parse runs on [`Self::parser`]
    /// instead of on this, because in the C the parse deliberately drives the
    /// same struct with a null dictionary and the port would rather say so in
    /// the type system.
    dec: Lzma2Decoder,
    parser: Lzma2Parser,

    /// C: `t->outBuf` / `t->outBufSize`. Owned here between blocks and lent to
    /// `dec` as its dictionary while a block is being decoded.
    out_buf: Vec<u8>,

    /// C: `t->state`.
    state: ParseState,
    /// C: `t->parseStatus`.
    parse_status: ParseStatus,

    /// C: `t->inPreSize` / `t->outPreSize`.
    in_pre_size: u64,
    out_pre_size: usize,
    /// C: `t->inCodeSize` / `t->outCodeSize`.
    in_code_size: u64,
    out_code_size: u64,
    /// C: `t->codeRes`.
    code_res: Option<Error>,
    /// The parse walked into a byte the format does not allow.
    parse_failed: bool,

    /// Absolute unpacked offset at which this block starts, captured from the
    /// parse's own running total while the read token is held - so it is in
    /// read order, which is write order, and is checked against the write
    /// side's total before any checksum derived from it is published.
    #[cfg(feature = "crc")]
    block_start: u64,
    /// What [`Coder::checksum`] computed, waiting to be published by
    /// [`Coder::write`] if this block is in fact written.
    #[cfg(feature = "crc")]
    checks: Option<BlockChecks>,
}

impl Lzma2Coder {
    /// C: the `if (!t->dec_created)` branch of `Lzma2DecMt_MtCallback_Parse`,
    /// hoisted to where the thread is created. `Lzma2Dec_AllocateProbs` (not
    /// `Lzma2Dec_Allocate`): a worker never owns a dictionary, because its
    /// dictionary is the output block it is about to fill.
    pub(crate) fn new(props: Lzma2CoderProps) -> Result<Self, Error> {
        let dec = Lzma2Decoder::new_probs_only(props.prop)?;
        Ok(Lzma2Coder {
            props,
            dec,
            parser: Lzma2Parser::new(),
            out_buf: Vec::new(),
            state: ParseState::Continue,
            parse_status: ParseStatus::NotSpecified,
            in_pre_size: 0,
            out_pre_size: 0,
            in_code_size: 0,
            out_code_size: 0,
            code_res: None,
            parse_failed: false,
            #[cfg(feature = "crc")]
            block_start: 0,
            #[cfg(feature = "crc")]
            checks: None,
        })
    }

    /// Reclaims the block buffer the decoder was lent, if it still has it.
    fn reclaim_out_buf(&mut self) {
        let b = self.dec.take_block_dic();
        if !b.is_empty() {
            self.out_buf = b;
        }
    }
}

impl Coder for Lzma2Coder {
    /// C: `Lzma2DecMt_MtCallback_Parse`.
    #[allow(clippy::too_many_lines)]
    fn parse(&mut self, cc: &mut CallbackInfo<'_>) {
        cc.state = ParseState::Continue;

        if cc.start_call {
            self.parser.init();
            self.in_pre_size = 0;
            self.out_pre_size = 0;
            self.parse_status = ParseStatus::NotSpecified;
            self.state = ParseState::Continue;
            self.in_code_size = 0;
            self.out_code_size = 0;
            self.code_res = None;
            self.parse_failed = false;
            #[cfg(feature = "crc")]
            {
                self.block_start = cc.out_processed_parse;
                self.checks = None;
            }
            // (cc->srcSize == 0) is allowed
        }

        let mut check_finish_block = true;
        let mut limit = self.props.out_block_max;
        if let Some(out_size) = self.props.out_size {
            let rem = out_size - cc.out_processed_parse;
            if limit as u64 >= rem {
                limit = rem as usize;
                if !self.props.finish_mode {
                    check_finish_block = false;
                }
            }
        }

        // checkFinishBlock = False, if we want to decode partial data
        // that must be finished at position <= outBlockMax.

        let mut status;
        let mut overflow;
        let mut unpack_rem: u32 = 0;
        {
            let src_orig = cc.src.len();
            let mut src_size_point = 0usize;
            let mut dic_pos_point = 0usize;

            cc.src_size = 0;
            overflow = false;

            loop {
                let (st, used) = self.parser.parse(
                    limit - self.parser.dic_pos,
                    &cc.src[cc.src_size..src_orig],
                    check_finish_block,
                );
                cc.src_size += used;
                status = st;

                if st == ParseStatus::NewChunk {
                    if self.parser.unpack_size() as usize
                        > self.props.out_block_max - self.parser.dic_pos
                    {
                        overflow = true;
                        break;
                    }
                    continue;
                }

                if st == ParseStatus::NewBlock {
                    if self.parser.dic_pos == 0 {
                        continue;
                    }
                    // we decode small blocks in one thread
                    if self.parser.dic_pos >= SMALL_BLOCK {
                        break;
                    }
                    dic_pos_point = self.parser.dic_pos;
                    src_size_point = cc.src_size;
                    continue;
                }

                if st == ParseStatus::NotFinished && check_finish_block {
                    overflow = true;
                    break;
                }

                unpack_rem = self.parser.unpack_extra();
                break;
            }

            if dic_pos_point != 0
                && status != ParseStatus::NewBlock
                && status != ParseStatus::FinishedWithMark
                && status != ParseStatus::NotSpecified
            {
                // we revert to latest newBlock state
                status = ParseStatus::NewBlock;
                unpack_rem = 0;
                self.parser.dic_pos = dic_pos_point;
                cc.src_size = src_size_point;
                overflow = false;
            }
        }

        self.in_pre_size += cc.src_size as u64;
        self.parse_status = status;
        if self.parser.errored() {
            self.parse_failed = true;
        }

        if overflow {
            cc.state = ParseState::Overflow;
        } else {
            let mut dic_pos = self.parser.dic_pos;

            if status != ParseStatus::NeedsMoreInput {
                if status == ParseStatus::NewBlock {
                    cc.state = ParseState::New;
                    // we don't need control byte of next block
                    cc.src_size -= 1;
                    self.in_pre_size -= 1;
                } else {
                    cc.state = ParseState::End;
                    if status != ParseStatus::FinishedWithMark {
                        // (status == NOT_SPECIFIED) or (status == NOT_FINISHED)
                        if unpack_rem != 0 {
                            // we also reserve space for the max possible number
                            // of output bytes of the current LZMA chunk
                            let mut rem = limit - dic_pos;
                            if rem > unpack_rem as usize {
                                rem = unpack_rem as usize;
                            }
                            dic_pos += rem;
                        }
                    }
                }

                cc.out_processed_parse += dic_pos as u64;
            }

            cc.out_pos = dic_pos as u64;
            self.out_pre_size = dic_pos;
        }

        self.state = cc.state;
    }

    /// C: `Lzma2DecMt_MtCallback_PreCode`.
    fn pre_code(&mut self) -> Result<(), MtError> {
        // A block that parsed no input at all began where the input ended.
        if self.in_pre_size == 0 {
            self.code_res = Some(Error::Truncated);
            return Err(MtError::Lzma(Error::Truncated));
        }

        // The block's buffer lives in one of two places: here between blocks,
        // and inside the decoder as its dictionary while one is being decoded.
        // `MtDec` may pre-code the same block twice - a block whose first
        // attempt did not consume its input is parsed and set up again - and
        // the buffer is then still the decoder's. Taking `out_buf` a second
        // time would hand the decoder an empty dictionary while telling it the
        // block is `out_pre_size` bytes long, which on the next uncompressed
        // chunk is a slice index past the end of an empty buffer. Found on
        // windows-msvc by the corrupt-run differential test, which is the
        // shape that makes `MtDec` retry.
        if self.out_buf.is_empty() {
            self.out_buf = self.dec.take_block_dic();
        }

        // C frees and reallocates when the buffer is too small. The port grows
        // it instead and lets `dic_buf_size` say how much of it this block
        // uses, so a stream of blocks of differing sizes does not re-zero the
        // whole buffer every time.
        if self.out_buf.len() < self.out_pre_size {
            let more = self.out_pre_size - self.out_buf.len();
            self.out_buf
                .try_reserve_exact(more)
                .map_err(|_| MtError::Lzma(Error::Alloc))?;
            self.out_buf.resize(self.out_pre_size, 0u8);
        }

        let buf = core::mem::take(&mut self.out_buf);
        self.dec.set_block_dic(buf, self.out_pre_size);
        Ok(())
    }

    /// C: `Lzma2DecMt_MtCallback_Code`.
    fn code(
        &mut self,
        src: &[u8],
        _src_finished: bool,
        in_code_pos: &mut u64,
        out_code_pos: &mut u64,
        stop: &mut bool,
    ) -> Result<(), MtError> {
        *in_code_pos = self.in_code_size;
        *out_code_pos = 0;
        *stop = true;

        let block_was_finished = self.parse_status == ParseStatus::FinishedWithMark
            || self.parse_status == ParseStatus::NewBlock;

        let finish = if block_was_finished {
            FinishMode::End
        } else {
            FinishMode::Any
        };

        let res = self.dec.decode_block(self.out_pre_size, src, finish);

        let src_processed = match res {
            Ok((n, _status)) => n,
            Err(e) => {
                self.code_res = Some(e);
                self.in_code_size += 0;
                self.out_code_size = self.dec.dic_pos() as u64;
                *out_code_pos = self.out_code_size;
                return Err(MtError::Lzma(e));
            }
        };

        self.in_code_size += src_processed as u64;
        *in_code_pos = self.in_code_size;
        self.out_code_size = self.dec.dic_pos() as u64;
        *out_code_pos = self.out_code_size;

        if src_processed == src.len() {
            *stop = false;
        }

        if block_was_finished {
            if src.len() != src_processed {
                return Err(MtError::Lzma(Error::InternalFailure));
            }
            if self.in_pre_size == self.in_code_size {
                if self.out_pre_size as u64 != self.out_code_size {
                    return Err(MtError::Lzma(Error::InternalFailure));
                }
                *stop = true;
            }
        } else if self.out_pre_size as u64 == self.out_code_size {
            *stop = true;
        }

        Ok(())
    }

    /// C: nothing. Runs on the worker thread after its block is decoded and
    /// *before* it queues for the write token, which is the whole point: the
    /// bytes are checksummed where they were produced, in parallel with every
    /// other worker, instead of on whoever drains the output.
    #[cfg(feature = "crc")]
    fn checksum(&mut self) {
        if self.props.checks.is_none()
            || self.props.plan.is_none()
            || self.code_res.is_some()
            || self.parse_failed
        {
            return;
        }
        // Take the block buffer back from the decoder early; `write` reclaims
        // it too and finds it already taken, which is a no-op there.
        self.reclaim_out_buf();
        let size = self.out_code_size as usize;
        let mut seg = Segmenter::new(&self.props.plan, self.block_start);
        seg.update(&self.out_buf[..size]);
        self.checks = Some(seg.finish());
    }

    /// C: `Lzma2DecMt_MtCallback_Write`.
    ///
    /// The `LZMA2DECMT_STREAM_WRITE_STEP` chunking of the C exists only so
    /// that `ICompressProgress` can be polled between 16 MiB pieces; without a
    /// progress callback the whole block goes out in one `write_all`.
    fn write(
        &mut self,
        ctx: WriteCtx<'_>,
        need_write_to_stream: bool,
        need_continue: &mut bool,
        can_recode: &mut bool,
    ) -> Result<(), MtError> {
        self.reclaim_out_buf();
        let size = self.out_code_size as usize;
        if self.parse_failed {
            return Err(MtError::Lzma(Error::CorruptData));
        }

        let mut need_continue2 = true;
        *need_continue = false;
        *can_recode = true;

        if self.state == ParseState::Overflow || self.state == ParseState::End {
            need_continue2 = false;
        }

        if !need_write_to_stream {
            return Ok(());
        }

        *ctx.in_processed += self.in_code_size;

        if self.code_res.is_none()
            && (self.parse_status == ParseStatus::FinishedWithMark
                || self.parse_status == ParseStatus::NewBlock)
            && (self.out_pre_size as u64 != self.out_code_size
                || self.in_pre_size != self.in_code_size)
        {
            return Err(MtError::Lzma(Error::InternalFailure));
        }

        *can_recode = false;

        #[cfg(feature = "crc")]
        if let (Some(sink), Some(checks)) = (self.props.checks.as_ref(), self.checks.take()) {
            // The segment offsets were derived from the parse's running output
            // total. That total is maintained under the read token and blocks
            // are written in read order, so it must equal the write side's -
            // and if it ever did not, every offset published from here would be
            // silently wrong, so it is checked rather than assumed.
            if checks.unpacked_offset != *ctx.out_processed || checks.len != size as u64 {
                return Err(MtError::Lzma(Error::InternalFailure));
            }
            if let Ok(mut v) = sink.lock() {
                v.push(checks);
            }
        }

        ctx.out.write_all(&self.out_buf[..size])?;
        *ctx.out_processed += size as u64;
        *need_continue = need_continue2;
        if self.parse_status == ParseStatus::FinishedWithMark {
            self.props.finished.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}
