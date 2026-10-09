//! The LZMA2 encoder.
//!
//! C: `CLzma2Enc` in `C/Lzma2Enc.c` — `Lzma2EncInt_EncodeSubblock`,
//! `Lzma2Enc_EncodeMt1`, `Lzma2EncProps_Normalize`, `Lzma2Enc_WriteProperties`
//! and, behind [`Lzma2Encoder::set_threads`], `Lzma2Enc_Encode2`'s `MtCoder`
//! path over [`crate::enc::mt_coder`].
//!
//! An LZMA2 stream is a series of *blocks*, each a run of chunks that opens by
//! resetting the dictionary and so decodes without reference to anything
//! before it. `blockSize` is how much input one block may cover, and its two
//! sentinels are the C's: [`BLOCK_SIZE_SOLID`] for one block over everything —
//! the default, and what this encoder did before threads existed — and
//! [`BLOCK_SIZE_AUTO`] for the size `Lzma2EncProps_Normalize` derives from the
//! dictionary.
//!
//! Blocks are the only parallelism the format has, and the output does not
//! depend on how many threads produced them: for one `(props, blockSize)` the
//! bytes are the same at one thread and at sixteen. `docs/encoder.md` says how
//! that is proved.

use alloc::vec::Vec;

#[cfg(feature = "std")]
use crate::enc::lz_find_mt;
use crate::enc::lzma_enc::LzmaEnc;
use crate::enc::props::LzmaEncProps;
#[cfg(feature = "std")]
use crate::enc::stream::NoStream;
use crate::enc::stream::{LimitedSeqInStream, SeqInStream, SeqOutStream, SliceStream};
use crate::error::Error;

#[cfg(feature = "std")]
use crate::enc::mt_coder::{BLOCKS_MAX, MtCoder, MtCoderCallback, MtInput, THREADS_MAX};
#[cfg(feature = "std")]
use std::sync::Mutex;

/// C: `LZMA2_CONTROL_LZMA`.
const CONTROL_LZMA: u8 = 1 << 7;
/// C: `LZMA2_CONTROL_COPY_NO_RESET`.
const CONTROL_COPY_NO_RESET: u8 = 2;
/// C: `LZMA2_CONTROL_COPY_RESET_DIC`.
const CONTROL_COPY_RESET_DIC: u8 = 1;
/// C: `LZMA2_CONTROL_EOF`.
pub(crate) const CONTROL_EOF: u8 = 0;

/// C: `LZMA2_PACK_SIZE_MAX`, which is also `LZMA2_COPY_CHUNK_SIZE`.
const PACK_SIZE_MAX: usize = 1 << 16;
/// C: `LZMA2_UNPACK_SIZE_MAX`, which is also `LZMA2_KEEP_WINDOW_SIZE`.
pub(crate) const UNPACK_SIZE_MAX: u32 = 1 << 21;
/// C: `LZMA2_CHUNK_SIZE_COMPRESSED_MAX`.
const CHUNK_SIZE_COMPRESSED_MAX: usize = (1 << 16) + 16;

/// C: `LZMA2_ENC_PROPS_BLOCK_SIZE_SOLID`. One block for the whole input.
pub const BLOCK_SIZE_SOLID: u64 = u64::MAX;
/// C: `LZMA2_ENC_PROPS_BLOCK_SIZE_AUTO`. The size [`auto_block_size`] derives
/// from the dictionary.
pub const BLOCK_SIZE_AUTO: u64 = 0;

/// The largest thread count [`Lzma2Encoder::set_threads`] will take. Without
/// `std` there are no threads at all and the encoder is always solid.
#[cfg(feature = "std")]
const THREADS_LIMIT: usize = THREADS_MAX;
#[cfg(not(feature = "std"))]
const THREADS_LIMIT: usize = 1;

/// C: `LZMA2_DIC_SIZE_FROM_PROP(p)`.
const fn dic_size_from_prop(p: u32) -> u32 {
    (2 | (p & 1)) << (p / 2 + 11)
}

/// The block size `Lzma2EncProps_Normalize` derives from a dictionary size.
///
/// C: the `LZMA2_ENC_PROPS_BLOCK_SIZE_AUTO` arm of `Lzma2EncProps_Normalize` —
/// four dictionaries, held between 1 MiB and 256 MiB, never below the
/// dictionary itself, rounded up to a whole megabyte.
#[must_use]
pub const fn auto_block_size(dict_size: u32) -> u64 {
    const K_MIN_SIZE: u64 = 1 << 20;
    const K_MAX_SIZE: u64 = 1 << 28;
    let mut block_size = (dict_size as u64) << 2;
    if block_size < K_MIN_SIZE {
        block_size = K_MIN_SIZE;
    }
    if block_size > K_MAX_SIZE {
        block_size = K_MAX_SIZE;
    }
    if block_size < dict_size as u64 {
        block_size = dict_size as u64;
    }
    block_size += K_MIN_SIZE - 1;
    block_size &= !(K_MIN_SIZE - 1);
    block_size
}

/// C: `CLzma2EncInt`, one block coder. The threaded path keeps one of these
/// per thread (`me->coders[coderIndex]`); the single-threaded path has one.
pub(crate) struct Lzma2EncInt {
    enc: alloc::boxed::Box<LzmaEnc>,
    /// C: `p->propsByte`, captured by `Lzma2EncInt_InitStream`.
    props_byte: u8,
    dict_size: u32,
    /// C: `p->needInitState`.
    need_init_state: bool,
    /// C: `p->needInitProp`.
    need_init_prop: bool,
    /// C: `p->srcPos`, the block's uncompressed position.
    src_pos: u64,
    /// C: `me->tempBufLzma`.
    temp: Vec<u8>,
}

impl Lzma2EncInt {
    /// C: `LzmaEnc_Create` plus `Lzma2EncInt_InitStream`'s property capture.
    pub(crate) fn new(props: &LzmaEncProps) -> Result<Self, Error> {
        let mut enc = alloc::boxed::Box::new(LzmaEnc::new()?);
        enc.set_props(props)?;
        let props_byte = enc.write_properties()[0];
        let dict_size = enc.dict_size;

        let mut temp = Vec::new();
        temp.try_reserve_exact(CHUNK_SIZE_COMPRESSED_MAX)
            .map_err(|_| Error::Alloc)?;

        Ok(Lzma2EncInt {
            enc,
            props_byte,
            dict_size,
            need_init_state: true,
            need_init_prop: true,
            src_pos: 0,
            temp,
        })
    }

    /// C: `LzmaEnc_SetProps` on the encoder this coder already has, and
    /// `Lzma2EncInt_InitStream`'s property capture again. The next block is
    /// encoded as a coder built with `props` would encode it: every setting
    /// the encoder keeps comes from `LzmaEnc_SetProps`, and everything else
    /// is set up again by the `LzmaEnc_Alloc` and `LzmaEnc_Init` that open
    /// each block.
    pub(crate) fn set_props(&mut self, props: &LzmaEncProps) -> Result<(), Error> {
        self.enc.set_props(props)?;
        self.props_byte = self.enc.write_properties()[0];
        self.dict_size = self.enc.dict_size;
        Ok(())
    }

    /// Whether the block loop's `LzmaEnc_Alloc` would accept the settings,
    /// without allocating.
    pub(crate) fn check_alloc(&mut self) -> Result<(), Error> {
        self.enc.check_alloc(UNPACK_SIZE_MAX)
    }

    /// How many times the match finder has allocated a window or its tables.
    #[cfg(test)]
    pub(crate) fn finder_allocs(&mut self) -> u32 {
        self.enc.mf.cfg().allocs
    }
}

/// How one LZMA2 block's subblock loop is run.
///
/// C has nothing here: its threaded match finder keeps `mf->stream` as a
/// pointer and reads it from the hash thread whatever the caller passed. This
/// port hands the hash thread the *stream itself*, inside a
/// `std::thread::scope`, so that the borrow is checked - and a stream can only
/// be handed over when it is [`Send`]. The two impls are what that distinction
/// costs: one for an input that can be sent, one for an input that cannot and
/// therefore always uses the single-threaded finder.
pub(crate) trait DriveBlock {
    /// Runs `coder`'s subblock loop over this block.
    ///
    /// # Errors
    ///
    /// Whatever the encoder or the streams return.
    fn drive(&mut self, coder: &mut Lzma2EncInt, out: &mut dyn SeqOutStream) -> Result<(), Error>;
}

impl<'b> DriveBlock for LimitedSeqInStream<'_, dyn SeqInStream + 'b> {
    fn drive(&mut self, coder: &mut Lzma2EncInt, out: &mut dyn SeqOutStream) -> Result<(), Error> {
        coder.subblock_loop(self, out)
    }
}

impl<'b> DriveBlock for LimitedSeqInStream<'_, dyn SeqInStream + Send + 'b> {
    fn drive(&mut self, coder: &mut Lzma2EncInt, out: &mut dyn SeqOutStream) -> Result<(), Error> {
        #[cfg(feature = "std")]
        if let Some(sh) = coder.enc.mt_handle() {
            // C: the window between `MatchFinderMt_Create` and
            // `MatchFinderMt_ReleaseStream`. The hash thread reads this block's
            // input; the encoder's own stream argument is never touched, which
            // is what `NoStream` says.
            return lz_find_mt::with_threads(&sh, self, || coder.subblock_loop(&mut NoStream, out));
        }
        coder.subblock_loop(self, out)
    }
}

impl Lzma2EncInt {
    /// C: `Lzma2EncInt_InitBlock`.
    fn init_block(&mut self) {
        self.src_pos = 0;
        self.need_init_state = true;
        self.need_init_prop = true;
    }

    /// The head of [`Self::encode_mt1_stream`]'s block loop for a
    /// [`BLOCK_SIZE_SOLID`] block whose length was never announced: what the
    /// push encoder starts its one block with. `data_limit` is the promise
    /// [`LzmaEnc::set_data_limit`] documents, which sizes the window and
    /// nothing else.
    pub(crate) fn begin_solid(&mut self, data_limit: u64) -> Result<(), Error> {
        self.init_block();
        self.enc.set_data_size(u64::MAX);
        self.enc.set_data_limit(data_limit);
        self.enc.prepare(UNPACK_SIZE_MAX)
    }

    /// C: `p->srcPos`, how much of the block has been encoded.
    pub(crate) fn src_pos(&self) -> u64 {
        self.src_pos
    }

    /// C: `Lzma2Enc_EncodeMt1`'s `inStream` path, the whole loop over blocks.
    ///
    /// `expected_data_size` is `me->expectedDataSize` and `finished` is the C's
    /// argument of that name: whether the end-of-stream control byte belongs at
    /// the end. `data_limit` is the most `input` can supply, `u64::MAX` when
    /// only the stream knows; see [`LzmaEnc::set_data_limit`].
    fn encode_mt1_stream<S>(
        &mut self,
        input: &mut S,
        out: &mut dyn SeqOutStream,
        block_size: u64,
        expected_data_size: u64,
        data_limit: u64,
        finished: bool,
    ) -> Result<(), Error>
    where
        S: SeqInStream + ?Sized,
        for<'a> LimitedSeqInStream<'a, S>: DriveBlock,
    {
        let mut unpack_total = 0u64;
        let mut limited = LimitedSeqInStream::new(input);
        loop {
            self.init_block();
            limited.reset(block_size);

            // C: `expected = me->expectedDataSize - unpackTotal`, clamped to
            // the block. It only sizes the hash table, but it does change the
            // bytes, so the memory path must agree with it.
            let mut expected = u64::MAX;
            if expected_data_size != u64::MAX && expected_data_size >= unpack_total {
                expected = expected_data_size - unpack_total;
            }
            if block_size != BLOCK_SIZE_SOLID && expected > block_size {
                expected = block_size;
            }
            self.enc.set_data_size(expected);
            // What this block can be given: the rest of the input, and never
            // more than `limited` lets through.
            let mut limit = data_limit.saturating_sub(unpack_total);
            if block_size != BLOCK_SIZE_SOLID {
                limit = limit.min(block_size);
            }
            self.enc.set_data_limit(limit);
            self.enc.prepare(UNPACK_SIZE_MAX)?;

            limited.drive(self, out)?;

            if self.src_pos != limited.processed {
                return Err(Error::InternalFailure);
            }
            unpack_total += self.src_pos;

            if limited.finished {
                if finished {
                    out.write(&[CONTROL_EOF])?;
                }
                return Ok(());
            }
        }
    }

    /// C: `Lzma2Enc_EncodeMt1`'s `inData` path, which is what the `MtCoder`
    /// callback drives: `src` is already at most one block, so the C's loop
    /// over blocks runs once.
    #[cfg(feature = "std")]
    pub(crate) fn encode_mt1_mem(
        &mut self,
        src: &[u8],
        out: &mut dyn SeqOutStream,
        finished: bool,
    ) -> Result<(), Error> {
        self.init_block();
        // C: `LzmaEnc_MemPrepare`, whose `MatchFinder_SET_DIRECT_INPUT_BUF`
        // also sets `expectedDataSize` from the block's length — which is what
        // makes this byte for byte what the stream path above produces for the
        // same block. `SliceStream` stands in for `directInput`; see
        // `crate::enc::stream`.
        let mut input = SliceStream::new(src);
        self.enc.set_data_limit(src.len() as u64);
        self.enc.mem_prepare(src.len() as u64, UNPACK_SIZE_MAX)?;
        match self.enc.mt_handle() {
            Some(sh) => lz_find_mt::with_threads(&sh, &mut input, || {
                self.subblock_loop(&mut NoStream, out)
            })?,
            None => self.subblock_loop(&mut input, out)?,
        }
        if self.src_pos != src.len() as u64 {
            return Err(Error::InternalFailure);
        }
        if finished {
            out.write(&[CONTROL_EOF])?;
        }
        Ok(())
    }

    /// C: the `for (;;) { Lzma2EncInt_EncodeSubblock(...) }` loop both
    /// `Lzma2Enc_EncodeMt1` paths run over one block.
    fn subblock_loop(
        &mut self,
        input: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        loop {
            let pack_size = self.encode_subblock(input, out)?;
            if pack_size == 0 {
                return Ok(());
            }
        }
    }

    /// C: `Lzma2EncInt_EncodeSubblock`, in the `outStream` form. Returns how
    /// many bytes it wrote, which is zero when the block is finished.
    pub(crate) fn encode_subblock(
        &mut self,
        input: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
    ) -> Result<usize, Error> {
        let lz_header_size = 5 + usize::from(self.need_init_prop);
        let mut unpack_size = UNPACK_SIZE_MAX;

        self.enc.save_state();
        self.temp.clear();
        let res = self.enc.code_one_mem_block(
            input,
            self.need_init_state,
            &mut self.temp,
            CHUNK_SIZE_COMPRESSED_MAX - lz_header_size,
            PACK_SIZE_MAX,
            &mut unpack_size,
        );
        // C: an output overflow is not an error here — it is the signal to
        // store the chunk instead.
        let overflowed = res?;
        let pack_size = self.temp.len();

        if unpack_size == 0 {
            return Ok(0);
        }

        // C: the chunk did not pay for itself (or did not fit), so store it.
        if overflowed || pack_size + 2 >= unpack_size as usize || pack_size > (1 << 16) {
            let mut written = 0usize;
            let mut remaining = unpack_size;
            // C: `LzmaEnc_GetCurBuf(p->enc) - unpackSize`.
            let mut at = self.enc.get_cur_buf() - unpack_size as usize;
            while remaining != 0 {
                let u = remaining.min(PACK_SIZE_MAX as u32);
                let control = if self.src_pos == 0 {
                    CONTROL_COPY_RESET_DIC
                } else {
                    CONTROL_COPY_NO_RESET
                };
                out.write(&[control, ((u - 1) >> 8) as u8, (u - 1) as u8])?;
                out.write(&self.enc.window()[at..at + u as usize])?;
                at += u as usize;
                remaining -= u;
                self.src_pos += u64::from(u);
                written += 3 + u as usize;
            }
            self.enc.restore_state();
            return Ok(written);
        }

        let u = unpack_size - 1;
        let pm = (pack_size - 1) as u32;
        // C: 3 resets the dictionary, 2 resets state and properties, 1 resets
        // state only, 0 continues.
        let mode: u8 = if self.src_pos == 0 {
            3
        } else if self.need_init_state {
            if self.need_init_prop { 2 } else { 1 }
        } else {
            0
        };

        let mut header = [0u8; 6];
        header[0] = CONTROL_LZMA | (mode << 5) | ((u >> 16) & 0x1F) as u8;
        header[1] = (u >> 8) as u8;
        header[2] = u as u8;
        header[3] = (pm >> 8) as u8;
        header[4] = pm as u8;
        if self.need_init_prop {
            header[5] = self.props_byte;
        }
        out.write(&header[..lz_header_size])?;
        out.write(&self.temp)?;

        self.need_init_prop = false;
        self.need_init_state = false;
        self.src_pos += u64::from(unpack_size);
        Ok(lz_header_size + pack_size)
    }
}

/// An LZMA2 encoder: blocks of LZMA2 chunks, ending in the end-of-stream
/// control byte.
///
/// C: `CLzma2EncHandle` driven by `Lzma2Enc_Encode2`.
pub struct Lzma2Encoder {
    props: LzmaEncProps,
    /// C: `me->coders[0]`, the coder the single-threaded path uses.
    pub(crate) coder: Lzma2EncInt,
    dict_size: u32,
    /// C: `props.blockSize`.
    block_size: u64,
    /// C: `props.numBlockThreads_Max`.
    threads: usize,
    /// The block thread count [`Lzma2Encoder::set_threads`] was last given,
    /// 1 if it never was: what `threads` goes back to when the total is
    /// cleared.
    named_threads: usize,
    /// C: `props.numTotalThreads`, or 0 when the caller has not set one.
    total_threads: usize,
    /// The memory the block threads may take together, `u64::MAX` for no
    /// limit.
    mem_limit: u64,
    /// C: `me->expectedDataSize`.
    expected_data_size: u64,
    /// What [`Lzma2Encoder::coder`] was built with, so that a change of block
    /// size can be noticed.
    coder_props: LzmaEncProps,
    /// Whether [`Lzma2Encoder::coder`] has been given other settings since it
    /// was built, and so may hold a window and tables larger than its
    /// settings need.
    coder_reconfigured: bool,
    /// C: `me->coders[1..]`, the coders of the block threads after the first;
    /// [`Lzma2Encoder::coder`] is the first thread's, as `me->coders[0]` is.
    /// Each is built when its thread first takes a block, and is kept from
    /// then on - window, tables and all - for every later block and stream.
    #[cfg(feature = "std")]
    block_coders: Vec<Mutex<Option<Lzma2EncInt>>>,
    /// The settings every coder in `block_coders` has.
    #[cfg(feature = "std")]
    block_coder_props: LzmaEncProps,
    /// Whether a coder in `block_coders` has been given other settings since
    /// it was built, as `coder_reconfigured` is for the first.
    #[cfg(feature = "std")]
    block_coders_reconfigured: bool,
}

impl Lzma2Encoder {
    /// C: `Lzma2Enc_Create` plus `Lzma2Enc_SetProps`.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if a setting is out of range — including `lc + lp`
    /// above 4, which LZMA2 does not allow — and [`Error::Alloc`] on
    /// allocation failure.
    pub fn new(props: &LzmaEncProps) -> Result<Self, Error> {
        // C: `Lzma2Enc_SetProps` refuses lc + lp above `LZMA2_LCLP_MAX`,
        // which is what an LZMA2 decoder is allowed to allocate for.
        props.check_lclp_for_lzma2()?;
        let coder = Lzma2EncInt::new(props)?;
        let dict_size = coder.dict_size;
        Ok(Lzma2Encoder {
            props: *props,
            coder,
            dict_size,
            block_size: BLOCK_SIZE_SOLID,
            threads: 1,
            named_threads: 1,
            total_threads: 0,
            mem_limit: u64::MAX,
            expected_data_size: u64::MAX,
            coder_props: *props,
            coder_reconfigured: false,
            #[cfg(feature = "std")]
            block_coders: Vec::new(),
            #[cfg(feature = "std")]
            block_coder_props: *props,
            #[cfg(feature = "std")]
            block_coders_reconfigured: false,
        })
    }

    /// The LZMA settings a block is actually encoded with.
    ///
    /// C: the `reduceSize` dance at the top of `Lzma2EncProps_Normalize` — a
    /// block smaller than the file is all the dictionary a block coder can
    /// ever see, so `LzmaEncProps_Normalize` is told to shrink the dictionary
    /// to it. A solid or automatic block size leaves the settings alone, which
    /// is why `Lzma2Encoder::new` can normalize before the block size is
    /// known.
    /// C: the `t1` / `t2` / `t3` arithmetic at the head of
    /// `Lzma2EncProps_Normalize`, as `(match finder threads, block threads)`.
    ///
    /// A caller who names block threads, or no thread count at all, gets one
    /// finder thread unless the settings ask for two: this port's default for
    /// `lzmaProps.numThreads` is 1 (see [`LzmaEncProps::with_num_threads`]).
    ///
    /// A caller who gives only a total has left the split to the encoder, and
    /// gets the C's: `t1n`, the finder's thread count, is the one the settings
    /// name, or 2 where they name none and the finder can take a thread of its
    /// own, and the total is divided by it. So a total of N over a binary tree
    /// in normal mode is N / 2 block coders, each with a threaded finder, and
    /// a total of N over a hash chain, or in fast mode, is N block coders.
    fn split_threads(&self) -> (usize, usize) {
        // C: `t1` is the raw setting, still -1 when the caller never named
        // one.
        let mut t1 = self.props.num_threads;
        let mut t2 = self.threads;
        let t3 = self.total_threads;

        if t3 == 0 {
            t2 = t2.max(1);
        } else if t2 == 0 {
            // C: `t1n`, `lzmaProps.numThreads` after a normalize of its own:
            // `(btMode && algo) ? 2 : 1` where the caller named none. The C
            // also divides by a count named for a finder that cannot thread;
            // here that finder counts as the one thread it runs on, and a
            // count above two as the two a finder can use.
            let mut normal = self.props;
            normal.normalize();
            let threaded_finder = normal.bt_mode != 0 && normal.algo != 0;
            let t1n: i32 = if !threaded_finder {
                1
            } else if t1 <= 0 {
                2
            } else {
                t1.min(2)
            };
            t2 = t3 / t1n as usize;
            if t2 == 0 {
                t1 = 1;
                t2 = t3;
            } else {
                t1 = t1n;
            }
            t2 = t2.min(THREADS_LIMIT);
        } else if t1 <= 0 {
            t1 = (t3 / t2).max(1) as i32;
        }
        // No total, or block threads named and the total spent on them: the
        // port's default of one finder thread.
        let mf = if t1 <= 0 { 1 } else { t1 };
        (mf.clamp(1, 2) as usize, t2.clamp(1, THREADS_LIMIT))
    }

    fn effective_props(&self) -> LzmaEncProps {
        let mut p = self.props;
        // C: `p->lzmaProps.numThreads = t1` before `LzmaEncProps_Normalize`.
        p.num_threads = self.split_threads().0 as i32;
        let bs = self.block_size;
        if bs != BLOCK_SIZE_SOLID
            && bs != BLOCK_SIZE_AUTO
            && (bs < p.reduce_size || p.reduce_size == u64::MAX)
        {
            p.reduce_size = bs;
        }
        p
    }

    /// Gives the single-threaded coder the settings
    /// [`Lzma2Encoder::effective_props`] resolves to, if the block size or the
    /// thread split has changed them since it last had them.
    ///
    /// C: `Lzma2Enc_SetProps` followed by `Lzma2EncInt_InitStream`, which
    /// calls `LzmaEnc_SetProps` on the encoder `Lzma2Enc_Create` made. The
    /// encoder is not built again: [`Lzma2Encoder::new`] built the only one.
    ///
    /// Under a memory limit it is, when it has been given other settings: a
    /// coder keeps the window and tables of the largest settings it has
    /// streamed with, and the limit pays for what these settings need, not
    /// for that.
    pub(crate) fn sync_coder(&mut self) -> Result<(), Error> {
        let want = self.effective_props();
        if want != self.coder_props {
            self.coder.set_props(&want)?;
            self.dict_size = self.coder.dict_size;
            self.coder_props = want;
            self.coder_reconfigured = true;
        }
        if self.coder_reconfigured && self.mem_limit != u64::MAX {
            self.coder = Lzma2EncInt::new(&want)?;
            self.coder_reconfigured = false;
        }
        Ok(())
    }

    /// C: `Lzma2Enc_SetProps` on a handle that has been used: `props` in place
    /// of the settings [`Lzma2Encoder::new`] was given. The block size, the
    /// thread counts, the memory limit and the data size stay as they are, and
    /// so does everything the encoder has allocated; the next stream is
    /// written as a new encoder with these settings would write it.
    ///
    /// # Errors
    ///
    /// As [`Lzma2Encoder::new`], with the encoder left as it was.
    pub(crate) fn set_props(&mut self, props: &LzmaEncProps) -> Result<(), Error> {
        props.check_lclp_for_lzma2()?;
        let old = core::mem::replace(&mut self.props, *props);
        // `LzmaEnc_SetProps` checks every setting before it takes any, so a
        // refusal has changed nothing in the coder either.
        if let Err(err) = self.sync_coder() {
            self.props = old;
            return Err(err);
        }
        Ok(())
    }

    /// The single LZMA2 property byte, as the `.xz` filter and the 7z coder
    /// carry it.
    ///
    /// C: `Lzma2Enc_WriteProperties`.
    #[must_use]
    pub fn properties(&self) -> u8 {
        let dict_size = self.dict_size();
        let mut i = 0u32;
        while i < 40 {
            if dict_size <= dic_size_from_prop(i) {
                break;
            }
            i += 1;
        }
        i as u8
    }

    /// The dictionary size the property byte rounds up to, with the block
    /// size's effect on it applied.
    #[must_use]
    pub fn dict_size(&self) -> u32 {
        self.effective_props().normalized().dict_size
    }

    /// C: `Lzma2Enc_SetDataSize`.
    pub fn set_data_size(&mut self, expected: u64) {
        self.expected_data_size = expected;
    }

    /// How much input one block may cover.
    ///
    /// C: `props.blockSize`. [`BLOCK_SIZE_SOLID`] is the default and is one
    /// block for the whole input; [`BLOCK_SIZE_AUTO`] is [`auto_block_size`]
    /// of the dictionary.
    pub fn set_block_size(&mut self, block_size: u64) {
        self.block_size = block_size;
    }

    /// The block size in force, with the sentinels resolved.
    ///
    /// C: `p->blockSize` after `Lzma2EncProps_Normalize`.
    #[must_use]
    pub fn block_size(&self) -> u64 {
        match self.block_size {
            BLOCK_SIZE_AUTO => {
                // C: `t2 <= 1`, the block threads the split above arrives at,
                // which for a caller who gave only a total is not a number
                // they set.
                if self.split_threads().1 <= 1 {
                    // C: "if there is no block multi-threading, we use SOLID
                    // block".
                    BLOCK_SIZE_SOLID
                } else {
                    auto_block_size(self.dict_size())
                }
            }
            other => other,
        }
    }

    /// How many threads may compress blocks at once.
    ///
    /// C: `props.numBlockThreads_Max`. One is the default and is the
    /// single-threaded path, byte for byte as before block threads existed.
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads.clamp(1, THREADS_LIMIT);
        self.named_threads = self.threads;
    }

    /// The total thread budget, block threads times match-finder threads.
    ///
    /// C: `props.numTotalThreads`. With this set and
    /// [`Lzma2Encoder::set_threads`] left alone, `Lzma2EncProps_Normalize`
    /// divides the budget: `numBlockThreads_Max = numTotalThreads /
    /// numThreads`. Setting it to zero goes back to "derive it from the block
    /// thread count", the one [`Lzma2Encoder::set_threads`] was last given.
    ///
    /// The divisor is the match finder's thread count: the one
    /// [`LzmaEncProps::with_num_threads`] named, or, where none was named, two
    /// for a binary-tree finder outside fast mode and one for a hash chain or
    /// for fast mode, whose finder cannot take a thread of its own. A total of
    /// `N` is therefore `N / 2` block coders with threaded finders in the
    /// first case and `N` block coders in the second. Blocks are only coded in
    /// parallel when a block size is set ([`Lzma2Encoder::set_block_size`]);
    /// under the default, one solid block, the total buys the threaded finder
    /// and nothing more.
    ///
    /// The total is not a count of operating-system threads. The C counts a
    /// threaded finder as two, and it runs on three - the coder, the hash
    /// thread and the tree thread - so a total of `N` over a binary tree
    /// starts about `1.5 N` threads.
    pub fn set_total_threads(&mut self, threads: usize) {
        self.total_threads = threads;
        self.threads = if threads != 0 { 0 } else { self.named_threads };
    }

    /// A ceiling on the memory the block threads may take together.
    ///
    /// C: the reduction 7-Zip applies for `mt` when `memUsage` is set — the
    /// thread count comes down until the estimate fits, and never below one.
    pub fn set_mem_limit(&mut self, bytes: u64) {
        self.mem_limit = bytes;
    }

    /// What one block thread is estimated to need, in bytes.
    ///
    /// The estimate is this port's own allocation arithmetic — the match
    /// finder's window and reference tables, the encoder's literal probability
    /// arrays, and the block's output buffer — not a formula taken from
    /// 7-Zip's C++.
    #[must_use]
    pub fn mem_usage_per_thread(&mut self) -> u64 {
        let block = match self.block_size() {
            BLOCK_SIZE_SOLID => u64::from(self.dict_size()),
            other => other,
        };
        // C: `destBlockSize` in `Lzma2Enc_Encode2`, plus the block's own copy
        // of the input that `MtCoder` holds.
        let bufs = block + (block >> 10) + 16 + block;
        let _ = self.sync_coder();
        self.coder.enc.mem_usage().saturating_add(bufs)
    }

    /// The thread count after the memory limit and the number of blocks have
    /// been applied.
    ///
    /// C: `numBlockThreads_Reduced`.
    #[must_use]
    pub fn threads_reduced(&mut self) -> usize {
        if self.block_size() == BLOCK_SIZE_SOLID {
            // C: a solid block is one block, so it is one thread.
            return 1;
        }
        let mut t = self.split_threads().1;
        if self.mem_limit != u64::MAX {
            let per = self.mem_usage_per_thread().max(1);
            let fits = (self.mem_limit / per) as usize;
            t = t.min(fits.max(1));
        }
        // C: "if (numBlocks < t2) t2r = numBlocks", once the data size is
        // known.
        if self.expected_data_size != u64::MAX {
            let num_blocks = self.expected_data_size.div_ceil(self.block_size()).max(1);
            if num_blocks < t as u64 {
                t = num_blocks as usize;
            }
        }
        t.max(1)
    }

    /// Encode `input` into `out` as one LZMA2 stream.
    ///
    /// C: `Lzma2Enc_Encode2`'s `outStream` path, which is single-threaded
    /// whatever [`Lzma2Encoder::set_threads`] says: the C's `MtCoder` path
    /// takes its input as memory. [`Lzma2Encoder::encode_slice`] is the one
    /// that threads.
    ///
    /// # Errors
    ///
    /// Whatever the streams return, or [`Error::Alloc`].
    pub fn encode(
        &mut self,
        input: &mut dyn SeqInStream,
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        self.sync_coder()?;
        let block_size = self.block_size();
        self.coder.encode_mt1_stream(
            input,
            out,
            block_size,
            self.expected_data_size,
            u64::MAX,
            true,
        )
    }

    /// [`Lzma2Encoder::encode`] for an input that can be handed to the
    /// threaded match finder.
    ///
    /// Same bytes, same C: the only difference is that `Send` lets
    /// `LzmaEncProps::with_num_threads(2)` actually start the finder's
    /// threads.
    ///
    /// # Errors
    ///
    /// As [`Lzma2Encoder::encode`].
    pub fn encode_send(
        &mut self,
        input: &mut (dyn SeqInStream + Send),
        out: &mut dyn SeqOutStream,
    ) -> Result<(), Error> {
        self.encode_send_limited(input, out, u64::MAX)
    }

    /// [`Lzma2Encoder::encode_send`] for an input known to supply at most
    /// `data_limit` bytes. The bytes are the same; the window is no longer
    /// than that input needs.
    fn encode_send_limited(
        &mut self,
        input: &mut (dyn SeqInStream + Send),
        out: &mut dyn SeqOutStream,
        data_limit: u64,
    ) -> Result<(), Error> {
        self.sync_coder()?;
        let block_size = self.block_size();
        self.coder.encode_mt1_stream(
            input,
            out,
            block_size,
            self.expected_data_size,
            data_limit,
            true,
        )
    }

    /// Encode a slice into one LZMA2 stream.
    ///
    /// C: `Lzma2Enc_Encode2` with `inData`, which is where the `MtCoder` path
    /// lives. With more than one thread and a block size other than
    /// [`BLOCK_SIZE_SOLID`] the blocks are compressed in parallel; the bytes
    /// are the same either way.
    ///
    /// # Errors
    ///
    /// Whatever the output stream returns, or [`Error::Alloc`].
    /// The sink must be [`Send`] because with more than one thread the worker
    /// that finished a block is the one that writes it, in order. C: the same
    /// `ISeqOutStream` is reached from `Lzma2Enc_MtCallback_Write` on whichever
    /// thread holds the write turn. Each block reaches the sink whole,
    /// through [`SeqOutStream::write_vec`], which a sink may override to keep
    /// the block's buffer instead of copying it.
    ///
    /// The encoder keeps each block thread's coder, with its window and
    /// tables, for the next block and the next stream; dropping the encoder
    /// is what releases them.
    pub fn encode_slice(
        &mut self,
        src: &[u8],
        out: &mut (dyn SeqOutStream + Send),
    ) -> Result<(), Error> {
        self.sync_coder()?;
        #[cfg(feature = "std")]
        {
            let threads = self.threads_reduced();
            if threads > 1 {
                return self.encode_slice_mt(src, out, threads);
            }
        }
        let mut input = SliceStream::new(src);
        self.encode_send_limited(&mut input, out, src.len() as u64)
    }

    /// Encode a slice into a fresh `Vec`.
    ///
    /// # Errors
    ///
    /// [`Error::Alloc`] if the output could not be grown.
    pub fn encode_to_vec(&mut self, src: &[u8]) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.set_data_size(src.len() as u64);
        self.encode_slice(src, &mut out)?;
        Ok(out)
    }

    /// Encode a stream with block threads.
    ///
    /// C: `Lzma2Enc_Encode2`'s `inStream` argument reaching `MtCoder`, which
    /// reads one block at a time on whichever thread holds the read token.
    ///
    /// Unlike [`Lzma2Encoder::encode_slice`], this does not in general produce
    /// what [`Lzma2Encoder::encode`] produces for the same input: a block read
    /// from a stream is encoded knowing its own length, whereas the
    /// single-threaded loop tells the encoder the block size until the stream
    /// runs out, and the expected data size changes how the match finder's
    /// hash table is sized. Give [`Lzma2Encoder::set_data_size`] the real
    /// length and the two agree. Either way the output is deterministic for a
    /// given `(props, block size, data size)` and does not depend on the
    /// thread count.
    ///
    /// # Errors
    ///
    /// Whatever the streams return, or [`Error::Alloc`].
    #[cfg(feature = "std")]
    pub fn encode_mt(
        &mut self,
        input: &mut (dyn SeqInStream + Send),
        out: &mut (dyn SeqOutStream + Send),
    ) -> Result<(), Error> {
        self.sync_coder()?;
        let threads = self.threads_reduced();
        if threads <= 1 {
            return self.encode_send(input, out);
        }
        self.run_mt(MtInput::Stream(Mutex::new(input)), out, threads)
    }

    /// C: the `p->props.numBlockThreads_Reduced > 1` arm of
    /// `Lzma2Enc_Encode2`.
    #[cfg(feature = "std")]
    fn encode_slice_mt(
        &mut self,
        src: &[u8],
        out: &mut (dyn SeqOutStream + Send),
        threads: usize,
    ) -> Result<(), Error> {
        self.run_mt(MtInput::Data(src), out, threads)
    }

    /// The body both threaded entry points share: line up the per-thread
    /// coders and the per-block output buffers, then hand them to `MtCoder`.
    #[cfg(feature = "std")]
    fn run_mt(
        &mut self,
        input: MtInput<'_>,
        out: &mut (dyn SeqOutStream + Send),
        threads: usize,
    ) -> Result<(), Error> {
        let block_size = usize::try_from(self.block_size()).map_err(|_| Error::Param)?;
        let expected_data_size = self.expected_data_size;
        let cb = self.mt_callback(out, threads)?;
        MtCoder {
            block_size,
            num_threads_max: threads,
            expected_data_size,
            input,
            callback: &cb,
        }
        .code()
    }

    /// What `MtCoder` drives for one stream on `threads` block threads.
    #[cfg(feature = "std")]
    fn mt_callback<'o>(
        &'o mut self,
        out: &'o mut (dyn SeqOutStream + Send),
        threads: usize,
    ) -> Result<Lzma2MtCallback<'o>, Error> {
        let props = self.coder_props;

        // C: `me->coders[i]`, one per block thread. `Lzma2Enc_EncodeMt1`
        // creates a coder's `CLzmaEnc` the first time that coder is given a
        // block, and so does this: a slot is empty until then. The ones an
        // earlier stream built are given this stream's settings, as
        // `Lzma2EncInt_InitStream` gives them.
        if self.block_coder_props != props {
            for slot in &mut self.block_coders {
                if let Some(coder) = slot.get_mut().unwrap_or_else(|e| e.into_inner()) {
                    coder.set_props(&props)?;
                    self.block_coders_reconfigured = true;
                }
            }
            self.block_coder_props = props;
        }
        // Under a memory limit, what the coders hold has to be what the limit
        // paid for: `threads` coders of these settings. The coders of threads
        // this stream does not run are released, and so is every coder that
        // was given other settings, which may hold the window and tables of
        // larger ones; their threads build them again.
        if self.mem_limit != u64::MAX {
            self.block_coders.truncate(threads.saturating_sub(1));
            if self.block_coders_reconfigured {
                for slot in &mut self.block_coders {
                    *slot.get_mut().unwrap_or_else(|e| e.into_inner()) = None;
                }
                self.block_coders_reconfigured = false;
            }
        }
        let more = threads
            .saturating_sub(1)
            .saturating_sub(self.block_coders.len());
        self.block_coders
            .try_reserve_exact(more)
            .map_err(|_| Error::Alloc)?;
        for _ in 0..more {
            self.block_coders.push(Mutex::new(None));
        }

        // C: `me->outBufs[i]`, one per block in flight, passed to `MtCoder`
        // by index. They are behind mutexes here, and each is given its
        // length by the block thread that first writes into it.
        let mut out_bufs = Vec::new();
        out_bufs
            .try_reserve_exact(BLOCKS_MAX)
            .map_err(|_| Error::Alloc)?;
        for _ in 0..BLOCKS_MAX {
            out_bufs.push(Mutex::new(Vec::new()));
        }

        Ok(Lzma2MtCallback {
            first: Mutex::new(&mut self.coder),
            rest: &self.block_coders,
            props,
            out_bufs,
            out: Mutex::new(out),
        })
    }
}

/// C: `Lzma2Enc_MtCallback_Code` and `Lzma2Enc_MtCallback_Write`.
#[cfg(feature = "std")]
struct Lzma2MtCallback<'o> {
    /// C: `me->coders[0]`.
    first: Mutex<&'o mut Lzma2EncInt>,
    /// C: `me->coders[1..]`; see [`Lzma2Encoder::block_coders`].
    rest: &'o [Mutex<Option<Lzma2EncInt>>],
    /// What a coder built during this stream is built with.
    props: LzmaEncProps,
    out_bufs: Vec<Mutex<Vec<u8>>>,
    out: Mutex<&'o mut (dyn SeqOutStream + Send)>,
}

#[cfg(feature = "std")]
impl MtCoderCallback for Lzma2MtCallback<'_> {
    /// C: `Lzma2Enc_MtCallback_Code`.
    fn code(
        &self,
        coder_index: usize,
        out_buf_index: usize,
        src: &[u8],
        finished: bool,
    ) -> Result<(), Error> {
        let mut dest = self.out_bufs[out_buf_index]
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        dest.clear();
        // C: `destBlockSize`, the `blockSize + (blockSize >> 10) + 16` every
        // `outBuf` is allocated at, here of this block's own length: more
        // than a block of all stored chunks comes to, taken once and not
        // reached by doubling. The buffer can still grow, so nothing rests
        // on the bound.
        let room = src.len().saturating_add((src.len() >> 10) + 16);
        if dest.capacity() < room {
            dest.try_reserve_exact(room).map_err(|_| Error::Alloc)?;
        }

        if coder_index == 0 {
            let mut coder = self.first.lock().unwrap_or_else(|e| e.into_inner());
            return coder.encode_mt1_mem(src, &mut *dest, finished);
        }
        let mut slot = self.rest[coder_index - 1]
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let coder = match &mut *slot {
            Some(coder) => coder,
            // C: `if (!p->enc) p->enc = LzmaEnc_Create(...)`, at the head
            // of `Lzma2Enc_EncodeMt1`.
            None => slot.insert(Lzma2EncInt::new(&self.props)?),
        };
        coder.encode_mt1_mem(src, &mut *dest, finished)
    }

    /// C: `Lzma2Enc_MtCallback_Write`.
    fn write(&self, out_buf_index: usize) -> Result<(), Error> {
        let mut data = self.out_bufs[out_buf_index]
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The whole block, in a buffer nothing else needs until the next
        // block that draws this index: the sink may keep it.
        self.out
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_vec(&mut data)
    }
}

/// Encode `src` as a raw LZMA2 stream, returning it with its property byte.
///
/// # Errors
///
/// [`Error::Param`] if a setting is out of range, [`Error::Alloc`] on
/// allocation failure.
pub fn encode_lzma2(src: &[u8], props: &LzmaEncProps) -> Result<(u8, Vec<u8>), Error> {
    let mut enc = Lzma2Encoder::new(props)?;
    let out = enc.encode_to_vec(src)?;
    Ok((enc.properties(), out))
}

/// Encode `src` as a raw LZMA2 stream with block threads.
///
/// `block_size` is how much input one block may cover ([`BLOCK_SIZE_AUTO`] to
/// take it from the dictionary), `threads` how many blocks may be compressed at
/// once. The bytes do not depend on `threads`.
///
/// # Errors
///
/// As [`encode_lzma2`].
pub fn encode_lzma2_mt(
    src: &[u8],
    props: &LzmaEncProps,
    block_size: u64,
    threads: usize,
) -> Result<(u8, Vec<u8>), Error> {
    let mut enc = Lzma2Encoder::new(props)?;
    enc.set_block_size(block_size);
    enc.set_threads(threads);
    let out = enc.encode_to_vec(src)?;
    Ok((enc.properties(), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_byte_rounds_the_dictionary_up() {
        // C: `LZMA2_DIC_SIZE_FROM_PROP(0)` is 1 << 12 and each step is the
        // next 2 or 3 times a power of two.
        assert_eq!(dic_size_from_prop(0), 1 << 12);
        assert_eq!(dic_size_from_prop(1), 3 << 11);
        assert_eq!(dic_size_from_prop(40 - 1), 3 << 30);
        for prop in 0..40u32 {
            let size = dic_size_from_prop(prop);
            let enc = Lzma2Encoder::new(&LzmaEncProps::new().with_dict_size(size)).unwrap();
            assert_eq!(u32::from(enc.properties()), prop, "dict size {size}");
        }
    }

    #[test]
    fn a_vec_of_zeros_becomes_one_lzma_chunk_and_an_end_marker() {
        let (_prop, out) = encode_lzma2(&[0u8; 4096], &LzmaEncProps::new()).unwrap();
        assert_eq!(out[0] & CONTROL_LZMA, CONTROL_LZMA, "an LZMA chunk");
        assert_eq!(out[0] >> 5 & 3, 3, "the first chunk resets the dictionary");
        assert_eq!(*out.last().unwrap(), CONTROL_EOF);
    }

    #[test]
    fn incompressible_input_falls_back_to_stored_chunks() {
        // A stored chunk is what the C writes when the packed size did not beat
        // the unpacked one; random bytes are the case that forces it.
        let src = pseudo_random(200_000);
        let (_prop, out) = encode_lzma2(&src, &LzmaEncProps::new()).unwrap();
        assert!(
            out.contains(&CONTROL_COPY_NO_RESET) || out[0] == CONTROL_COPY_RESET_DIC,
            "expected at least one stored chunk"
        );
        assert!(out.len() > src.len() / 2);
    }

    /// Reusing one encoder for several independent streams must not carry
    /// state across, whatever the sizes are.
    #[test]
    fn reuse_across_streams_of_different_sizes() {
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let mut enc = Lzma2Encoder::new(&props).expect("new");
        let src: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let _ = enc.encode_to_vec(&src).expect("whole");
        for chunk in src.chunks(16 * 1024) {
            let _ = enc.encode_to_vec(chunk).expect("encode");
        }
    }

    /// A solid block is what a fresh encoder writes, as it did before block
    /// threads existed.
    #[test]
    fn solid_is_still_the_default() {
        let mut enc = Lzma2Encoder::new(&LzmaEncProps::new()).unwrap();
        assert_eq!(enc.block_size(), BLOCK_SIZE_SOLID);
        assert_eq!(enc.threads_reduced(), 1);
    }

    /// Blocking the input is the same work whether one thread or several did
    /// it: same block size, same bytes.
    #[test]
    #[cfg(feature = "std")]
    fn threads_do_not_change_the_bytes() {
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let src = pseudo_random(300_000);
        for block_size in [1u64 << 14, 1 << 16, 1 << 20, 400_000, 100_000] {
            let mut one = Lzma2Encoder::new(&props).unwrap();
            one.set_block_size(block_size);
            let want = one.encode_to_vec(&src).unwrap();
            for threads in [2usize, 3, 8] {
                let mut enc = Lzma2Encoder::new(&props).unwrap();
                enc.set_block_size(block_size);
                enc.set_threads(threads);
                let got = enc.encode_to_vec(&src).unwrap();
                assert_eq!(got, want, "block {block_size}, {threads} threads");
            }
        }
    }

    /// An exact multiple of the block size has no trailing partial block, and
    /// an empty input is still a well-formed stream.
    #[test]
    #[cfg(feature = "std")]
    fn block_boundaries_and_empty_input() {
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        for len in [0usize, 1, 65_536, 131_072] {
            let src = pseudo_random(len);
            let mut one = Lzma2Encoder::new(&props).unwrap();
            one.set_block_size(1 << 16);
            let want = one.encode_to_vec(&src).unwrap();
            let mut enc = Lzma2Encoder::new(&props).unwrap();
            enc.set_block_size(1 << 16);
            enc.set_threads(4);
            assert_eq!(enc.encode_to_vec(&src).unwrap(), want, "len {len}");
            assert_eq!(*want.last().unwrap(), CONTROL_EOF, "len {len}");
        }
    }

    /// Told the real data size, the threaded stream path agrees with the
    /// single-threaded one byte for byte.
    #[test]
    #[cfg(feature = "std")]
    fn the_threaded_stream_path_matches_the_single_threaded_one() {
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let src = pseudo_random(250_000);
        for block_size in [1u64 << 15, 1 << 16, 120_000] {
            let mut one = Lzma2Encoder::new(&props).unwrap();
            one.set_block_size(block_size);
            one.set_data_size(src.len() as u64);
            let mut want = Vec::new();
            one.encode(&mut SliceStream::new(&src), &mut want).unwrap();

            for threads in [2usize, 5] {
                let mut enc = Lzma2Encoder::new(&props).unwrap();
                enc.set_block_size(block_size);
                enc.set_threads(threads);
                enc.set_data_size(src.len() as u64);
                let mut got = Vec::new();
                enc.encode_mt(&mut SliceStream::new(&src), &mut got)
                    .unwrap();
                assert_eq!(got, want, "block {block_size}, {threads} threads");
            }
        }
    }

    /// The memory limit brings the thread count down, and never below one.
    #[test]
    fn the_memory_limit_reduces_the_thread_count() {
        let props = LzmaEncProps::new().with_dict_size(1 << 20);
        let mut enc = Lzma2Encoder::new(&props).unwrap();
        enc.set_block_size(1 << 22);
        enc.set_threads(16);
        enc.set_data_size(1 << 30);
        let per = enc.mem_usage_per_thread();
        assert!(per > 0);
        enc.set_mem_limit(per * 4);
        assert_eq!(enc.threads_reduced(), 4);
        enc.set_mem_limit(1);
        assert_eq!(enc.threads_reduced(), 1);
    }

    /// The estimate covers what a block's coder really allocates, with and
    /// without the threaded match finder, whose `hashBuf` and `btBuf` and
    /// wider window it has to count. The allocation is read off the buffers
    /// the coder holds after `Lzma2EncInt_InitStream` has prepared it.
    #[test]
    fn the_estimate_bounds_what_a_block_coder_allocates() {
        // C: `kHashBufferSize + kBtBufferSize`, in `u32` words.
        const MT_BUFS: u64 = ((1 << 17) * 2 + (1 << 16) * 16) * 4;
        for dict_size in [1u32 << 16, 1 << 20, 1 << 23] {
            let mut per_finder = [0u64; 2];
            for mf_threads in [1u32, 2] {
                let props = LzmaEncProps::new()
                    .with_dict_size(dict_size)
                    .with_num_threads(mf_threads);
                let mut enc = Lzma2Encoder::new(&props).unwrap();
                enc.set_block_size(u64::from(dict_size) * 4);
                enc.set_threads(4);
                let what = format!("dict {dict_size}, {mf_threads} finder threads");

                let block = enc.block_size();
                let per = enc.mem_usage_per_thread();
                let estimate = enc.coder.enc.mem_usage();
                assert_eq!(per, estimate + block + (block >> 10) + 16 + block, "{what}");

                enc.coder.enc.prepare(UNPACK_SIZE_MAX).unwrap();
                let allocated = enc.coder.enc.allocated();
                assert_eq!(
                    enc.coder.enc.mf.is_mt(),
                    cfg!(feature = "std") && mf_threads == 2,
                    "{what}"
                );
                assert!(
                    estimate >= allocated,
                    "{what}: estimated {estimate}, allocated {allocated}"
                );
                // A bound, and a close one: nothing but rounding is spare.
                assert!(
                    estimate - allocated < 1 << 12,
                    "{what}: estimated {estimate}, allocated {allocated}"
                );
                per_finder[mf_threads as usize - 1] = per;
            }
            if cfg!(feature = "std") {
                assert!(
                    per_finder[1] >= per_finder[0] + MT_BUFS,
                    "dict {dict_size}: {per_finder:?}"
                );
            }
        }
    }

    /// The thread count also comes down to the number of blocks there are.
    #[test]
    fn fewer_blocks_than_threads_reduces_the_thread_count() {
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let mut enc = Lzma2Encoder::new(&props).unwrap();
        enc.set_block_size(1 << 16);
        enc.set_threads(16);
        enc.set_data_size(3 * (1 << 16));
        assert_eq!(enc.threads_reduced(), 3);
    }

    /// A total and nothing else leaves the split to the encoder: half the
    /// total in block coders where the finder takes a thread of its own, the
    /// whole total where it cannot. Naming block threads, or no thread count
    /// at all, still gets the one finder thread this port defaults to.
    #[test]
    fn a_total_alone_is_split_by_what_the_finder_can_use() {
        use crate::enc::MatchFinderKind;

        let blocks = |n: usize| n.min(THREADS_LIMIT);
        let tree = LzmaEncProps::new().with_level(5);
        let chain = tree.with_match_finder(MatchFinderKind::Hc4);
        let fast = LzmaEncProps::new().with_level(1);
        // Fast mode defaults to a hash chain; this names the tree back.
        let fast_tree = tree
            .with_fast_mode(true)
            .with_match_finder(MatchFinderKind::Bt4);

        // (settings, total) -> (match finder threads, block threads)
        let by_total: [(&str, LzmaEncProps, usize, (usize, usize)); 21] = [
            ("tree", tree, 1, (1, 1)),
            ("tree", tree, 2, (2, 1)),
            ("tree", tree, 3, (2, 1)),
            ("tree", tree, 4, (2, 2)),
            ("tree", tree, 8, (2, 4)),
            ("tree", tree, 18, (2, 9)),
            ("tree, two named", tree.with_num_threads(2), 1, (1, 1)),
            ("tree, two named", tree.with_num_threads(2), 8, (2, 4)),
            ("tree, eight named", tree.with_num_threads(8), 8, (2, 4)),
            ("tree, one named", tree.with_num_threads(1), 1, (1, 1)),
            ("tree, one named", tree.with_num_threads(1), 8, (1, 8)),
            ("chain", chain, 1, (1, 1)),
            ("chain", chain, 2, (1, 2)),
            ("chain", chain, 8, (1, 8)),
            ("chain, two named", chain.with_num_threads(2), 8, (1, 8)),
            ("fast", fast, 1, (1, 1)),
            ("fast", fast, 4, (1, 4)),
            ("fast", fast, 18, (1, 18)),
            ("fast tree", fast_tree, 2, (1, 2)),
            ("fast tree", fast_tree, 8, (1, 8)),
            (
                "fast tree, two named",
                fast_tree.with_num_threads(2),
                8,
                (1, 8),
            ),
        ];
        for (name, props, total, (finder, block)) in by_total {
            let mut enc = Lzma2Encoder::new(&props).unwrap();
            enc.set_total_threads(total);
            assert_eq!(
                enc.split_threads(),
                (finder, blocks(block)),
                "{name}, a total of {total}"
            );
            // The coder is given the finder's share.
            assert_eq!(enc.effective_props().num_threads, finder as i32, "{name}");
            // C: "if there is no block multi-threading, we use SOLID block".
            enc.set_block_size(BLOCK_SIZE_AUTO);
            assert_eq!(
                enc.block_size() == BLOCK_SIZE_SOLID,
                blocks(block) <= 1,
                "{name}, a total of {total}"
            );
        }

        // No total: the finder's default is one thread, whatever the kind.
        for (props, finder) in [
            (tree, 1),
            (tree.with_num_threads(2), 2),
            (chain, 1),
            (fast, 1),
        ] {
            let mut enc = Lzma2Encoder::new(&props).unwrap();
            assert_eq!(enc.split_threads(), (finder, 1));
            enc.set_threads(6);
            assert_eq!(enc.split_threads(), (finder, blocks(6)));
        }

        // A total with the block threads named: what is left over goes to the
        // finder, as it always did.
        for (total, threads, finder) in [(4usize, 2usize, 2usize), (4, 4, 1), (2, 4, 1)] {
            let mut enc = Lzma2Encoder::new(&tree).unwrap();
            enc.set_total_threads(total);
            enc.set_threads(threads);
            assert_eq!(enc.split_threads(), (finder, blocks(threads)));
        }

        // Clearing a total goes back to the block threads named before it,
        // or to one where none were.
        for named in [None, Some(4usize)] {
            let mut enc = Lzma2Encoder::new(&tree).unwrap();
            if let Some(threads) = named {
                enc.set_threads(threads);
            }
            let before = enc.split_threads();
            enc.set_total_threads(8);
            assert_eq!(enc.split_threads(), (2, blocks(4)));
            enc.set_total_threads(0);
            assert_eq!(enc.split_threads(), before);
            assert_eq!(before, (1, blocks(named.unwrap_or(1))));
        }
    }

    /// A total alone writes what its split writes when the caller names it:
    /// one solid block where one block coder is all it comes to, the automatic
    /// block size where it comes to more.
    #[test]
    #[cfg(feature = "std")]
    fn a_total_alone_writes_what_its_split_writes_when_named() {
        use std::io::Read as _;

        // Three blocks at the automatic size for this dictionary.
        let src = mixed((2 << 20) + 5);
        let tree = LzmaEncProps::new().with_level(5).with_dict_size(1 << 16);
        let fast = LzmaEncProps::new().with_level(1).with_dict_size(1 << 16);
        for (props, total) in [(tree, 1usize), (tree, 2), (tree, 4), (fast, 1), (fast, 2)] {
            let mut by_total = Lzma2Encoder::new(&props).unwrap();
            by_total.set_block_size(BLOCK_SIZE_AUTO);
            by_total.set_total_threads(total);
            let (finder, block) = by_total.split_threads();

            let mut named = Lzma2Encoder::new(&props.with_num_threads(finder as u32)).unwrap();
            named.set_block_size(BLOCK_SIZE_AUTO);
            named.set_threads(block);

            assert_eq!(by_total.block_size(), named.block_size(), "{total}");
            assert_eq!(by_total.block_size() == BLOCK_SIZE_SOLID, block == 1);
            let got = by_total.encode_to_vec(&src).unwrap();
            assert_eq!(by_total.properties(), named.properties(), "{total}");
            assert!(got == named.encode_to_vec(&src).unwrap(), "{total}");

            let mut back = Vec::new();
            crate::Lzma2Reader::new(&got[..], by_total.properties())
                .unwrap()
                .read_to_end(&mut back)
                .unwrap();
            assert!(back == src, "{total}");
        }
    }

    /// `sync_coder` gives the encoder `new` built its new settings; it does
    /// not build another.
    #[test]
    fn a_change_of_settings_keeps_the_encoder() {
        let props = LzmaEncProps::new().with_dict_size(1 << 20);
        let mut enc = Lzma2Encoder::new(&props).unwrap();
        let built: *const LzmaEnc = &*enc.coder.enc;
        // Not what `new` was given: the block size shrinks the dictionary and
        // the split names a thread count.
        enc.set_block_size(1 << 16);
        enc.sync_coder().unwrap();
        assert!(core::ptr::eq(built, &*enc.coder.enc));
        assert_eq!(enc.coder.dict_size, 1 << 16);
        assert_eq!(enc.dict_size(), 1 << 16);
        let _ = enc.encode_to_vec(&mixed(100_000)).unwrap();
        assert!(core::ptr::eq(built, &*enc.coder.enc));
    }

    /// An encoder that has already run with other settings writes what a
    /// fresh one writes: nothing of the earlier dictionary, match finder or
    /// thread split is left behind.
    #[test]
    fn an_encoder_given_new_settings_writes_what_a_fresh_one_writes() {
        let src = mixed(300_000);
        let base = LzmaEncProps::new().with_level(5).with_dict_size(1 << 20);
        // (block size, total threads, block threads): each step changes the
        // dictionary, the finder's thread count, or both.
        let steps: [(u64, usize, usize); 7] = [
            (BLOCK_SIZE_SOLID, 0, 1),
            (1 << 16, 0, 1),
            (1 << 18, 2, 1),
            (1 << 16, 4, 2),
            (1 << 14, 0, 1),
            (1 << 18, 2, 1),
            (BLOCK_SIZE_SOLID, 0, 1),
        ];
        let set = |enc: &mut Lzma2Encoder, (block, total, threads): (u64, usize, usize)| {
            enc.set_block_size(block);
            enc.set_total_threads(total);
            enc.set_threads(threads);
        };
        let mut reused = Lzma2Encoder::new(&base).unwrap();
        for step in steps {
            set(&mut reused, step);
            let got = reused.encode_to_vec(&src).unwrap();
            let mut fresh = Lzma2Encoder::new(&base).unwrap();
            set(&mut fresh, step);
            assert_eq!(fresh.properties(), reused.properties(), "{step:?}");
            assert!(got == fresh.encode_to_vec(&src).unwrap(), "{step:?}");
        }
    }

    /// A slice is all the input there will be, so the window is cut to it;
    /// what is written is what the window the C would have allocated writes.
    #[test]
    fn a_known_input_length_shortens_the_window_and_not_the_stream() {
        let whole = mixed((3 << 20) + 5);
        let lens = [
            0usize,
            1,
            2,
            100,
            4095,
            4096,
            65_535,
            65_536,
            65_537,
            200_000,
            (2 << 20) - 1,
            2 << 20,
            (2 << 20) + 1,
            whole.len(),
        ];
        for dict_size in [1u32 << 12, 1 << 16, 1 << 22] {
            for mf_threads in [1u32, 2] {
                let props = LzmaEncProps::new()
                    .with_level(5)
                    .with_dict_size(dict_size)
                    .with_num_threads(mf_threads);
                for len in lens {
                    let src = &whole[..len];
                    let what =
                        format!("dict {dict_size}, {mf_threads} finder threads, {len} bytes");

                    // The stream entry is told the length as a hint, which is
                    // what sizes the hash table, and nothing about a limit.
                    let mut full = Lzma2Encoder::new(&props).unwrap();
                    full.set_data_size(len as u64);
                    let mut want = Vec::new();
                    full.encode_send(&mut SliceStream::new(src), &mut want)
                        .unwrap();
                    let full_window = u64::from(full.coder.enc.mf.cfg().block_size);

                    let mut cut = Lzma2Encoder::new(&props).unwrap();
                    cut.set_data_size(len as u64);
                    let mut got = Vec::new();
                    cut.encode_slice(src, &mut got).unwrap();
                    assert!(got == want, "{what}");

                    let window = u64::from(cut.coder.enc.mf.cfg().block_size);
                    assert!(window <= full_window, "{what}");
                    // The input, the look-ahead (which for the threaded finder
                    // includes a block of hash heads) and the alignment.
                    assert!(
                        window <= len as u64 + (1 << 18),
                        "{what}: a window of {window}"
                    );
                }
            }
        }
    }

    /// The promise is what keeps the short window from ever being moved. A
    /// stream that breaks it ends in an error, not in an index out of the
    /// window.
    #[test]
    fn a_stream_longer_than_promised_is_an_error() {
        let src = mixed(1 << 20);
        for mf_threads in [1u32, 2] {
            let props = LzmaEncProps::new()
                .with_dict_size(1 << 16)
                .with_num_threads(mf_threads);
            let mut enc = Lzma2Encoder::new(&props).unwrap();
            enc.sync_coder().unwrap();
            let mut stream = SliceStream::new(&src);
            let input: &mut (dyn SeqInStream + Send) = &mut stream;
            let mut out = Vec::new();
            let res =
                enc.coder
                    .encode_mt1_stream(input, &mut out, BLOCK_SIZE_SOLID, u64::MAX, 10, true);
            assert!(res.is_err(), "{mf_threads} finder threads");
        }
    }

    /// One block after another and one stream after another on the threaded
    /// finder: the window, the tables and the hand-off buffers are allocated
    /// for the first block and for no other, and what is written is what an
    /// encoder that has never run writes.
    #[cfg(feature = "std")]
    #[test]
    fn the_threaded_finder_allocates_for_its_first_block_only() {
        let whole = mixed(900_000);
        let props = LzmaEncProps::new()
            .with_level(5)
            .with_dict_size(1 << 18)
            .with_num_threads(2);
        // (block size, input): six blocks, then a smaller dictionary over
        // other bytes, then the first dictionary again. Every later window
        // and table fits inside the first.
        let streams: [(u64, &[u8]); 4] = [
            (1 << 17, &whole[..700_000]),
            (1 << 16, &whole[300_000..750_000]),
            (1 << 17, &whole[100_000..900_000]),
            (1 << 17, &whole[..1]),
        ];
        let mut reused = Lzma2Encoder::new(&props).unwrap();
        for (i, (block, src)) in streams.into_iter().enumerate() {
            reused.set_block_size(block);
            let mut got = Vec::new();
            reused
                .encode_send(&mut SliceStream::new(src), &mut got)
                .unwrap();
            assert!(reused.coder.enc.mf.is_mt(), "stream {i}");
            assert_eq!(reused.coder.enc.mf.cfg().allocs, 3, "stream {i}");

            let mut fresh = Lzma2Encoder::new(&props).unwrap();
            fresh.set_block_size(block);
            let mut want = Vec::new();
            fresh
                .encode_send(&mut SliceStream::new(src), &mut want)
                .unwrap();
            assert!(got == want, "stream {i}");
        }
    }

    /// A block thread's coder is built when that thread first has a block,
    /// and is the same coder for every stream after: with new settings it
    /// writes what a coder built with them writes.
    #[cfg(feature = "std")]
    #[test]
    fn a_block_threads_coder_is_built_once_and_when_it_is_needed() {
        fn built(enc: &mut Lzma2Encoder) -> Vec<Option<*const LzmaEnc>> {
            enc.block_coders
                .iter_mut()
                .map(|slot| {
                    slot.get_mut()
                        .unwrap()
                        .as_ref()
                        .map(|coder| core::ptr::from_ref::<LzmaEnc>(&coder.enc))
                })
                .collect()
        }

        let src = mixed(200_000);
        for mf_threads in [1u32, 2] {
            let props = LzmaEncProps::new()
                .with_level(5)
                .with_dict_size(1 << 20)
                .with_num_threads(mf_threads);
            let mut enc = Lzma2Encoder::new(&props).unwrap();
            let first: *const LzmaEnc = &*enc.coder.enc;
            let mut kept = None;
            // The second stream has a smaller dictionary, so the coder the
            // first one built is given other settings.
            for block in [1usize << 16, 1 << 15, 1 << 16] {
                enc.set_block_size(block as u64);
                enc.set_threads(4);
                enc.sync_coder().unwrap();
                let (head, tail) = src.split_at(block);
                let tail = &tail[..block / 2];

                let mut got = Vec::new();
                {
                    let cb = enc.mt_callback(&mut got, 4).unwrap();
                    // The third block thread takes the first block, the
                    // first thread the last; the other two never run.
                    cb.code(2, 5, head, false).unwrap();
                    cb.code(0, 1, tail, true).unwrap();
                    cb.write(5).unwrap();
                    cb.write(1).unwrap();
                }
                assert!(core::ptr::eq(first, &*enc.coder.enc));
                let now = built(&mut enc);
                assert_eq!(now.len(), 3);
                assert!(now[0].is_none() && now[2].is_none());
                assert!(now[1].is_some());
                assert_eq!(*kept.get_or_insert(now[1]), now[1]);

                let mut fresh = Lzma2Encoder::new(&props).unwrap();
                fresh.set_block_size(block as u64);
                let want = fresh.encode_to_vec(&src[..block + block / 2]).unwrap();
                assert!(got == want, "{mf_threads} finder threads, block {block}");
            }
        }
    }

    /// Under a memory limit, an encoder that was used with more block threads
    /// or a larger dictionary keeps no more than the limit pays for: the
    /// coders of threads a later stream does not run are released, and a
    /// coder given smaller settings does not keep the window and tables of
    /// the larger ones.
    #[cfg(feature = "std")]
    #[test]
    fn a_memory_limit_releases_what_earlier_streams_built() {
        /// The window each coder holds, the first thread's first; `None` for
        /// a block thread whose coder has not been built.
        fn windows(enc: &mut Lzma2Encoder) -> Vec<Option<u64>> {
            let mut held = vec![Some(enc.coder.enc.mf.cfg().allocated())];
            for slot in &mut enc.block_coders {
                held.push(
                    slot.get_mut()
                        .unwrap()
                        .as_mut()
                        .map(|coder| coder.enc.mf.cfg().allocated()),
                );
            }
            held
        }

        let src = mixed(1 << 17);
        let big = LzmaEncProps::new().with_level(5).with_dict_size(1 << 20);
        let small = big.with_dict_size(1 << 12);
        let block = 1usize << 15;

        let mut enc = Lzma2Encoder::new(&big).unwrap();
        enc.set_block_size(block as u64);
        enc.set_threads(4);
        enc.sync_coder().unwrap();
        // Every one of the four block threads codes a block, so every coder
        // is built at the large dictionary.
        {
            let mut out = Vec::new();
            let cb = enc.mt_callback(&mut out, 4).unwrap();
            for (t, piece) in src.chunks(block).enumerate() {
                cb.code(t, t, piece, t == 3).unwrap();
            }
        }
        let large = windows(&mut enc);
        assert_eq!(large.len(), 4);
        assert!(large.iter().all(Option::is_some));

        // What a coder built for the small dictionary holds.
        let want = {
            let mut fresh = Lzma2Encoder::new(&small).unwrap();
            fresh.set_block_size(block as u64);
            fresh.sync_coder().unwrap();
            let mut out = Vec::new();
            let cb = fresh.mt_callback(&mut out, 1).unwrap();
            cb.code(0, 0, &src[..block], true).unwrap();
            drop(cb);
            fresh.coder.enc.mf.cfg().allocated()
        };
        assert!(want < large[0].unwrap(), "{want} {large:?}");

        // The small dictionary, and a limit that pays for two of its coders.
        enc.set_props(&small).unwrap();
        let per = enc.mem_usage_per_thread();
        enc.set_mem_limit(2 * per);
        let threads = enc.threads_reduced();
        assert_eq!(threads, 2);
        {
            let mut out = Vec::new();
            let cb = enc.mt_callback(&mut out, threads).unwrap();
            cb.code(0, 0, &src[..block], false).unwrap();
            cb.code(1, 1, &src[block..2 * block], true).unwrap();
        }
        let now = windows(&mut enc);
        assert!(now.len() <= threads, "{now:?}: coders kept past {threads}");
        for (i, held) in now.into_iter().enumerate() {
            let held = held.unwrap_or(0);
            assert!(
                held <= want,
                "coder {i} holds {held}, the settings need {want}"
            );
        }
    }

    /// A block's output buffer is as long as a block of that length can come
    /// to before the first byte goes into it, and stays that long.
    #[cfg(feature = "std")]
    #[test]
    fn a_blocks_output_buffer_is_sized_once() {
        // Nothing here compresses, so the block comes out longer than it
        // went in.
        let src = pseudo_random(100_000);
        let room = src.len() + (src.len() >> 10) + 16;
        let mut enc = Lzma2Encoder::new(&LzmaEncProps::new().with_dict_size(1 << 16)).unwrap();
        enc.set_block_size(1 << 17);
        enc.sync_coder().unwrap();
        let mut out = Vec::new();
        let cb = enc.mt_callback(&mut out, 2).unwrap();
        cb.code(0, 0, &src, true).unwrap();
        let (len, cap) = {
            let buf = cb.out_bufs[0].lock().unwrap();
            (buf.len(), buf.capacity())
        };
        assert!(len > src.len() && len <= room);
        // What was reserved, not the next power of two above the length.
        assert!((room..room + room / 8).contains(&cap), "{cap} for {room}");

        // The same buffer takes a second, shorter block as it is.
        cb.write(0).unwrap();
        cb.code(0, 0, &src[..1000], true).unwrap();
        assert_eq!(cb.out_bufs[0].lock().unwrap().capacity(), cap);
    }

    /// A sink that keeps what it is handed is handed every block whole, once,
    /// and in order; one that does not is written the same bytes.
    #[cfg(feature = "std")]
    #[test]
    fn a_finished_block_is_handed_to_the_sink_whole() {
        #[derive(Default)]
        struct Keeps {
            blocks: Vec<Vec<u8>>,
            writes: usize,
        }
        impl SeqOutStream for Keeps {
            fn write(&mut self, data: &[u8]) -> Result<(), Error> {
                self.writes += 1;
                self.blocks.push(data.to_vec());
                Ok(())
            }
            fn write_vec(&mut self, data: &mut Vec<u8>) -> Result<(), Error> {
                self.blocks.push(core::mem::take(data));
                Ok(())
            }
        }

        let src = mixed(300_000);
        let props = LzmaEncProps::new().with_level(5).with_dict_size(1 << 16);
        let mut enc = Lzma2Encoder::new(&props).unwrap();
        enc.set_block_size(1 << 16);
        enc.set_threads(3);

        let mut copied = Vec::new();
        enc.encode_slice(&src, &mut copied).unwrap();

        let mut kept = Keeps::default();
        enc.encode_slice(&src, &mut kept).unwrap();
        assert_eq!(kept.writes, 0);
        assert_eq!(kept.blocks.len(), src.len().div_ceil(1 << 16));
        assert!(kept.blocks.concat() == copied);

        // The default leaves the caller its buffer, emptied.
        let mut sink = Vec::new();
        let mut block = Vec::with_capacity(64);
        block.extend_from_slice(b"one block");
        SeqOutStream::write_vec(&mut sink, &mut block).unwrap();
        assert_eq!(sink, b"one block");
        assert!(block.is_empty() && block.capacity() >= 64);
    }

    /// Runs that compress and runs that do not, so that long matches, short
    /// ones and stored chunks all turn up.
    fn mixed(len: usize) -> Vec<u8> {
        let noise = pseudo_random(len);
        (0..len)
            .map(|i| {
                if (i / 5000) % 5 == 4 {
                    noise[i]
                } else {
                    ((i % 251) as u8).wrapping_add((i / 4093) as u8) ^ noise[i / 97]
                }
            })
            .collect()
    }

    /// Bytes that do not compress, so the stored-chunk path and the block
    /// boundaries both get exercised.
    fn pseudo_random(len: usize) -> Vec<u8> {
        let mut x = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }
}
