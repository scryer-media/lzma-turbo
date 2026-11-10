//! Multi-threaded LZMA2 decoding.
//!
//! C: `C/Lzma2DecMt.c` and `C/Lzma2DecMt.h`, over the generic framework in
//! `C/MtDec.c` ([`mtdec`]).
//!
//! An LZMA2 stream is a series of chunks, and a chunk whose control byte asks
//! for a dictionary reset starts a *block* that decodes without reference to
//! anything before it. That is the only parallelism there is in the format:
//! an encoder that never resets the dictionary produces a stream that cannot
//! be decoded on more than one thread, and `7zz -mmt=1` produces exactly such
//! a stream. Everything here is about finding those boundaries in a stream
//! that is being read, not seeked, and keeping every thread busy without
//! letting the amount of decoded-but-not-yet-written data grow without bound.
//!
//! See [`mtdec`] for the ring of threads that does the work; this module is
//! the LZMA2-specific half plus the public API and the single-threaded tail.

pub mod adaptive;
mod at;
pub use at::ReadAt;
#[cfg(feature = "crc")]
pub mod checksum;
pub(crate) mod event;
mod lzma2;
mod mtdec;
mod pool;
pub(crate) mod segq;
// Only the encoder's `MtCoder` port uses these; the decoder's ring needs
// nothing but `Event`.
#[cfg(feature = "enc")]
pub(crate) mod sync;

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

#[cfg(feature = "crc")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "crc")]
use checksum::{BlockChecks, ChecksumPlan, Segmenter};

use crate::error::{Error, FinishMode, Status};
use crate::lzma2::Lzma2Decoder;
use crate::lzma2::frame::dic_size_from_prop_full;
use crate::window::WindowBacking;

use lzma2::{Lzma2Coder, Lzma2CoderProps};
use mtdec::{MtDec, ReplayBuf};

/// C: `props.inBufSize_ST` from `Lzma2DecMtProps_Init`.
const IN_BUF_SIZE_ST: usize = 1 << 20;
/// C: `props.outStep_ST`.
const OUT_STEP_ST: usize = 1 << 20;
/// C: `props.inBufSize_MT`.
const IN_BUF_SIZE_MT: usize = 1 << 18;
/// C: `LZMA2DECMT_OUT_BLOCK_MAX_DEFAULT`.
const OUT_BLOCK_MAX_DEFAULT: usize = 1 << 28;
/// C: `kOverheadSize` in `CPP/7zip/Compress/Lzma2Decoder.cpp`.
const OVERHEAD_SIZE: u64 = IN_BUF_SIZE_MT as u64 + (1 << 16);

/// C: `Get_ExpectedBlockSize_From_Dict` in
/// `CPP/7zip/Compress/Lzma2Decoder.cpp`. The size 7-Zip's LZMA2 encoder uses
/// for its blocks, which is therefore the size the decoder should expect one
/// to be.
fn expected_block_size(dict_size: u32) -> u64 {
    const K_MIN_SIZE: u64 = 1 << 20;
    const K_MAX_SIZE: u64 = 1 << 28;
    let mut block_size = u64::from(dict_size) << 2;
    // Left as the C's two comparisons rather than a `clamp`: the next two
    // clauses are the same shape and only make sense read together.
    #[allow(clippy::manual_clamp)]
    if block_size < K_MIN_SIZE {
        block_size = K_MIN_SIZE;
    }
    if block_size > K_MAX_SIZE {
        block_size = K_MAX_SIZE;
    }
    if block_size < u64::from(dict_size) {
        block_size = u64::from(dict_size);
    }
    block_size += K_MIN_SIZE - 1;
    block_size &= !(K_MIN_SIZE - 1);
    block_size
}

impl Lzma2Decoder {
    /// The most bytes a threaded decoder asks a [`WindowAllocator`] for at
    /// once, for one worker or for its single-threaded path, for a stream
    /// whose dictionary-size property is `dict_prop`.
    ///
    /// It is the larger of the block 7-Zip's LZMA2 encoder makes for that
    /// dictionary (four times the dictionary, held to between 1 MiB and
    /// 256 MiB, and never less than the dictionary, rounded up to a whole
    /// MiB) and the dictionary buffer itself, [`Lzma2Decoder::window_len`].
    /// A worker decodes a block or run into a lent window only when it is at
    /// most this long; a longer run is decoded on the single-threaded path,
    /// whose buffer is the dictionary. So with a [`WindowBacking`], every
    /// window [`Lzma2ParallelDecoder`], [`Lzma2ParallelReader`],
    /// [`crate::Lzma2AdaptiveDecoder`] and [`crate::run::Lzma2RunDecoder`]
    /// ask for is at most this long, and the bytes lent at once are at most
    /// their `max_windows()` times this (one window for the run decoder),
    /// whatever the stream holds.
    ///
    /// For a 7z or `xz`-style stream whose dictionary is 64 MiB that is
    /// 256 MiB; for 256 MiB, 256 MiB; for 1 GiB, 1 GiB; and for 1.5 GiB,
    /// 1.5 GiB. 7-Zip's threaded encoder cuts blocks of exactly the first
    /// figure, so each of its blocks fits a worker's window.
    ///
    /// [`WindowAllocator`]: crate::WindowAllocator
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProps`] if `dict_prop > 40`.
    pub fn max_worker_window_len(dict_prop: u8) -> Result<usize, Error> {
        let window = Self::window_len(dict_prop)?;
        let block = expected_block_size(dic_size_from_prop_full(dict_prop));
        Ok(usize::try_from(block).unwrap_or(usize::MAX).max(window))
    }
}

/// How a multi-threaded decode is to be run.
///
/// C: `CLzma2DecMtProps`, reduced to the two knobs a caller actually turns.
/// The buffer sizes the C exposes are fixed here at the values
/// `Lzma2DecMtProps_Init` sets and 7-Zip never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lzma2MtOptions {
    /// Upper bound on worker threads. One means "decode single-threaded".
    pub threads: usize,
    /// Upper bound, in bytes, on the memory the decoder may use for blocks in
    /// flight. The thread count is reduced until the plan fits, exactly as
    /// 7-Zip's `CDecoder::Code` reduces it against `_memUsage`.
    pub memory_limit: u64,
}

impl Default for Lzma2MtOptions {
    /// As many threads as the machine reports, and no memory cap.
    ///
    /// A cap of [`u64::MAX`] means the thread count is whatever was asked for;
    /// [`Lzma2ParallelDecoder::memory_estimate`] then says what that will
    /// cost, and a caller that cares sets a limit instead of guessing.
    fn default() -> Self {
        Lzma2MtOptions {
            threads: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
            memory_limit: u64::MAX,
        }
    }
}

/// Where finished [`BlockChecks`] are put by whichever thread produced them.
///
/// The lock is taken once per output block, inside the ring's write section,
/// which is already serialised - so it adds a lock acquisition per 128 MiB of
/// output and no contention. The *checksumming* is emphatically not done here;
/// see [`crate::checksum`].
#[cfg(feature = "crc")]
pub(crate) type ChecksumSink = Arc<Mutex<Vec<BlockChecks>>>;

/// The sizes a decode will actually run with.
///
/// C: the block of `CLzma2DecMtProps` assignments in `CDecoder::Code`.
#[derive(Debug, Clone, Copy)]
struct Plan {
    threads: usize,
    out_block_max: usize,
    /// What one worker holds in RAM. A block buffer over a backing's in-RAM
    /// limit is lent, so with a backing only the limit's worth of it counts.
    per_thread: u64,
    st_only: bool,
}

fn plan(dict_prop: u8, options: &Lzma2MtOptions, window: Option<&WindowBacking>) -> Plan {
    let dict_size = dic_size_from_prop_full(dict_prop);
    let expected = expected_block_size(dict_size);
    let in_block_max = expected + expected / 16;

    // C: the `expectedBlockSize == expectedBlockSize64` guard, which is how
    // the 32-bit build declines to plan a block it cannot address.
    let fits = usize::try_from(expected).is_ok() && usize::try_from(in_block_max).is_ok();
    if !fits || options.threads <= 1 {
        return Plan {
            threads: 1,
            out_block_max: OUT_BLOCK_MAX_DEFAULT,
            per_thread: 0,
            st_only: true,
        };
    }

    // A block over the backing's ceiling could not be lent, so with a
    // backing blocks are cut no larger than it: a run that does not fit goes
    // to the single-threaded tail, whose buffer is only the dictionary.
    let out_block_max = match window {
        Some(w) if w.covers(expected as usize) => expected.min(w.max_len()),
        _ => expected,
    };
    // A worker's block buffer is at most `out_block_max` bytes. One over the
    // backing's in-RAM limit is lent, so what stays in RAM is at most the
    // limit; with no backing it is all of it, as 7-Zip counts it.
    let out_ram = match window {
        Some(w) if w.covers(out_block_max as usize) => out_block_max.min(w.in_memory_limit()),
        _ => out_block_max,
    };
    let per_thread = out_ram + in_block_max + OVERHEAD_SIZE;
    let ok_threads = options.memory_limit / per_thread;
    let mut threads = options.threads as u64;
    if threads > ok_threads {
        threads = ok_threads;
    }
    if threads == 0 {
        threads = 1;
    }
    Plan {
        threads: threads as usize,
        out_block_max: out_block_max as usize,
        per_thread,
        st_only: threads <= 1,
    }
}

/// A prepared multi-threaded LZMA2 decode.
///
/// Holds no decoder state: it is the plan, and each call to
/// [`Lzma2ParallelDecoder::decode`] runs a ring of threads that exists only
/// for the duration of that call.
#[derive(Debug, Clone)]
pub struct Lzma2ParallelDecoder {
    dict_prop: u8,
    plan: Plan,
    window: Option<WindowBacking>,
}

impl Lzma2ParallelDecoder {
    /// Plans a decode of a raw LZMA2 stream whose dictionary-size property
    /// byte is `dict_prop`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProps`] if `dict_prop > 40`.
    pub fn new(dict_prop: u8, options: &Lzma2MtOptions) -> Result<Self, Error> {
        if dict_prop > 40 {
            return Err(Error::UnsupportedProps);
        }
        Ok(Lzma2ParallelDecoder {
            dict_prop,
            plan: plan(dict_prop, options, None),
            window: None,
        })
    }

    /// As [`Lzma2ParallelDecoder::new`], with every dictionary buffer over
    /// the window's in-RAM limit on the window's allocator.
    ///
    /// Those buffers count nothing against `options.memory_limit`, so the
    /// thread count is reduced only for what stays in RAM, and
    /// [`Lzma2ParallelDecoder::memory_estimate`] reports only that. Each
    /// worker of [`Lzma2ParallelDecoder::decode`] holds one block buffer for
    /// its whole life and asks for a window only when its next block needs
    /// more than the buffer it has; [`Lzma2ParallelDecoder::decode_at`]
    /// holds one per run in flight and reuses a written run's for the next.
    /// A block or run longer than [`Lzma2Decoder::max_worker_window_len`]
    /// goes to no worker: `decode` gives it to the single-threaded path,
    /// and `decode_at` hands the rest of the stream to `decode`, after
    /// releasing every window it holds. The single-threaded tail asks for
    /// its dictionary only after the workers' windows are released. At most
    /// [`Lzma2ParallelDecoder::max_windows`] are alive at once.
    ///
    /// A buffer over [`WindowBacking::max_len`] fails the decode with the
    /// memory-limit error, and an allocator's refusal fails it with the
    /// allocator's error, as [`crate::Lzma2Reader::with_window`] reports
    /// both. Neither drops the decode to fewer threads or to RAM.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProps`] if `dict_prop > 40`.
    pub fn with_window(
        dict_prop: u8,
        options: &Lzma2MtOptions,
        window: WindowBacking,
    ) -> Result<Self, Error> {
        if dict_prop > 40 {
            return Err(Error::UnsupportedProps);
        }
        Ok(Lzma2ParallelDecoder {
            dict_prop,
            plan: plan(dict_prop, options, Some(&window)),
            window: Some(window),
        })
    }

    /// The most windows the backing can have lent to one decode at once:
    /// one per thread for the threaded pass, or one for a decode that runs
    /// single-threaded. Zero with no backing, or when no buffer this decode
    /// can allocate is over the in-RAM limit. Each window is at most
    /// [`Lzma2Decoder::max_worker_window_len`] and at most
    /// [`WindowBacking::max_len`] bytes, so the bytes lent at once are at
    /// most this times the smaller of the two.
    #[must_use]
    pub fn max_windows(&self) -> usize {
        let Some(w) = self.window.as_ref() else {
            return 0;
        };
        // The single-threaded tail's dictionary, lent only after the
        // threaded pass has released every window of its own.
        let tail = Lzma2Decoder::window_len(self.dict_prop).is_ok_and(|len| w.covers(len));
        if self.plan.st_only {
            return usize::from(tail);
        }
        // Any worker may hold one: a block, or a `decode_at` run, of up to
        // `max_worker_window_len` bytes.
        self.plan.threads
    }

    /// How many threads the plan will use. One means the single-threaded
    /// decoder runs directly, with no threads created and no overhead.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.plan.threads
    }

    /// An upper bound on the bytes this decode will allocate.
    #[must_use]
    ///
    /// Memory lent through a [`WindowBacking`] is not counted: this is what
    /// the decode holds in RAM.
    pub fn memory_estimate(&self) -> u64 {
        memory_estimate(
            self.dict_prop,
            self.plan.threads,
            self.plan.per_thread,
            self.window.as_ref(),
        )
    }

    /// Decodes `input` to `out` and returns the number of bytes written.
    ///
    /// C: `Lzma2DecMt_Decode`.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from either stream, and returns an [`Error`] as
    /// the `io::Error` of its class for a stream that does not decode:
    /// [`std::io::ErrorKind::InvalidData`] for corrupt data,
    /// [`std::io::ErrorKind::UnexpectedEof`] for a truncated stream.
    pub fn decode<R: Read + Send, W: Write + Send>(&self, input: R, out: W) -> io::Result<u64> {
        let mut input = input;
        let mut out = out;
        #[cfg(feature = "crc")]
        return self.decode_dyn(&mut input, &mut out, &ChecksumPlan::none(), None);
        #[cfg(not(feature = "crc"))]
        return self.decode_dyn(&mut input, &mut out);
    }

    /// Decodes `input` to `out`, checksumming each block in the worker that
    /// produced it, and returns the bytes written together with what was
    /// computed.
    ///
    /// The returned [`BlockChecks`] are in output order and tile the whole
    /// output: one per block the threaded pass wrote, plus one covering the
    /// single-threaded tail if there was one. Fold their segments into the
    /// ranges you care about with [`crate::crc::CrcFolder`].
    ///
    /// This costs the decode nothing that a consumer computing the same
    /// checksum would not cost it more: see [`crate::checksum`] for why.
    ///
    /// # Errors
    ///
    /// As [`Lzma2ParallelDecoder::decode`].
    #[cfg(feature = "crc")]
    pub fn decode_checksummed<R: Read + Send, W: Write + Send>(
        &self,
        input: R,
        out: W,
        plan: &ChecksumPlan,
    ) -> io::Result<(u64, Vec<BlockChecks>)> {
        let mut input = input;
        let mut out = out;
        let sink: ChecksumSink = Arc::new(Mutex::new(Vec::new()));
        let n = self.decode_to_sink(&mut input, &mut out, plan, &sink)?;
        let checks = Arc::try_unwrap(sink)
            .map(|m| m.into_inner().unwrap_or_default())
            .unwrap_or_default();
        Ok((n, checks))
    }

    /// As [`Lzma2ParallelDecoder::decode_checksummed`], but publishing into a
    /// sink the caller already holds, so it can be drained while the decode is
    /// still running.
    #[cfg(feature = "crc")]
    pub(crate) fn decode_to_sink<R: Read + Send, W: Write + Send>(
        &self,
        input: R,
        out: W,
        plan: &ChecksumPlan,
        sink: &ChecksumSink,
    ) -> io::Result<u64> {
        let mut input = input;
        let mut out = out;
        self.decode_dyn(&mut input, &mut out, plan, Some(sink))
    }

    fn decode_dyn(
        &self,
        input: &mut (dyn Read + Send),
        out: &mut (dyn Write + Send),
        #[cfg(feature = "crc")] plan: &ChecksumPlan,
        #[cfg(feature = "crc")] sink: Option<&ChecksumSink>,
    ) -> io::Result<u64> {
        if self.plan.st_only {
            let mut written = 0u64;
            return decode_st(
                self.dict_prop,
                self.window.as_ref(),
                input,
                out,
                VecDeque::new(),
                false,
                &mut written,
                #[cfg(feature = "crc")]
                plan,
                #[cfg(feature = "crc")]
                sink,
            );
        }

        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let props = Lzma2CoderProps {
            prop: self.dict_prop,
            out_block_max: self.plan.out_block_max,
            out_size: None,
            finish_mode: false,
            finished: std::sync::Arc::clone(&finished),
            #[cfg(feature = "crc")]
            plan: plan.clone(),
            #[cfg(feature = "crc")]
            checks: sink.cloned(),
            window: self.window.clone(),
        };
        let make = || Lzma2Coder::new(props.clone());

        let (replay, read_was_finished, mut written) = {
            let mt: MtDec<'_, Lzma2Coder> =
                MtDec::new(IN_BUF_SIZE_MT, self.plan.threads, input, out, &make);
            let r = mt.run().map_err(io::Error::from)?;
            if let Some(e) = r.read_res {
                return Err(e);
            }
            (r.replay, r.read_was_finished, r.out_processed)
        };

        if replay.is_empty() && read_was_finished {
            if !finished.load(std::sync::atomic::Ordering::Relaxed) {
                // The threaded pass consumed the whole stream without reaching
                // an end marker: the stream ended early, as the single-threaded
                // tail reports it.
                return Err(to_io(Error::Truncated));
            }
            return Ok(written);
        }

        // C: `MtDec_Code` returned with `needContinue`, so the stream is
        // finished single-threaded, starting from the input the threaded pass
        // read but did not decode.
        decode_st(
            self.dict_prop,
            self.window.as_ref(),
            input,
            out,
            replay,
            read_was_finished,
            &mut written,
            #[cfg(feature = "crc")]
            plan,
            #[cfg(feature = "crc")]
            sink,
        )?;
        Ok(written)
    }
}

/// An upper bound on the memory a decode with this plan will allocate in RAM.
fn memory_estimate(
    dict_prop: u8,
    threads: usize,
    per_thread: u64,
    window: Option<&WindowBacking>,
) -> u64 {
    let dict_size = u64::from(dic_size_from_prop_full(dict_prop));
    // The tail's dictionary is lent when the backing covers it.
    let lent =
        window.is_some_and(|w| Lzma2Decoder::window_len(dict_prop).is_ok_and(|len| w.covers(len)));
    let dict_ram = if lent { 0 } else { dict_size };
    // The single-threaded tail can run after the threaded pass, and its
    // dictionary and input buffer are additional to whatever the threaded pass
    // is still holding for replay.
    let st = dict_ram + IN_BUF_SIZE_ST as u64;
    if threads <= 1 {
        st
    } else {
        threads as u64 * per_thread + st
    }
}

/// An upper bound on the memory [`Lzma2ParallelDecoder`] would allocate for
/// these options, without building one.
///
/// With a [`WindowBacking`], build the decoder with
/// [`Lzma2ParallelDecoder::with_window`] and ask its
/// [`Lzma2ParallelDecoder::memory_estimate`], which counts only RAM.
///
/// # Errors
///
/// Returns [`Error::UnsupportedProps`] if `dict_prop > 40`.
pub fn mt_memory_estimate(dict_prop: u8, options: &Lzma2MtOptions) -> Result<u64, Error> {
    Ok(Lzma2ParallelDecoder::new(dict_prop, options)?.memory_estimate())
}

// ---------------------------------------------------------------------------
// The single-threaded tail
// ---------------------------------------------------------------------------

/// Input for the single-threaded pass: first whatever the threaded pass read
/// and did not use, then the stream itself.
///
/// C: `MtDec_PrepareRead` / `MtDec_Read`, then `ISeqInStream_Read`.
struct StSource<'a> {
    replay: VecDeque<ReplayBuf>,
    stream: &'a mut (dyn Read + Send),
    buf: Vec<u8>,
    cur: Option<Box<[u8]>>,
    pos: usize,
    lim: usize,
    read_was_finished: bool,
}

impl StSource<'_> {
    /// Ensures at least one unread byte is available, if there is one.
    fn fill(&mut self) -> io::Result<()> {
        if self.pos != self.lim {
            return Ok(());
        }
        if let Some(r) = self.replay.pop_front() {
            self.pos = 0;
            self.lim = r.len;
            self.cur = Some(r.buf);
            return Ok(());
        }
        self.cur = None;
        self.pos = 0;
        self.lim = 0;
        if self.read_was_finished {
            return Ok(());
        }
        if self.buf.is_empty() {
            let mut v = Vec::new();
            v.try_reserve_exact(IN_BUF_SIZE_ST)
                .map_err(|_| to_io(Error::Alloc))?;
            v.resize(IN_BUF_SIZE_ST, 0u8);
            self.buf = v;
        }
        let mut filled = 0usize;
        while filled < self.buf.len() {
            match self.stream.read(&mut self.buf[filled..]) {
                Ok(0) => {
                    self.read_was_finished = true;
                    break;
                }
                Ok(n) => filled += n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.lim = filled;
                    return Err(e);
                }
            }
        }
        self.lim = filled;
        Ok(())
    }

    fn slice(&self) -> &[u8] {
        match &self.cur {
            Some(b) => &b[self.pos..self.lim],
            None => &self.buf[self.pos..self.lim],
        }
    }
}

/// C: `Lzma2Dec_Decode_ST`.
#[allow(clippy::too_many_arguments)]
fn decode_st(
    dict_prop: u8,
    window: Option<&WindowBacking>,
    stream: &mut (dyn Read + Send),
    out: &mut (dyn Write + Send),
    replay: VecDeque<ReplayBuf>,
    read_was_finished: bool,
    written: &mut u64,
    #[cfg(feature = "crc")] plan: &ChecksumPlan,
    #[cfg(feature = "crc")] sink: Option<&ChecksumSink>,
) -> io::Result<u64> {
    let mut dec = tail_decoder(dict_prop, window)?;

    // The single-threaded path checksums too, so that a stream with no
    // dictionary resets - or the tail of one the threaded pass could not
    // finish - still produces segments covering every byte. There is no
    // serialised section to keep it out of here: this is the only thread.
    #[cfg(feature = "crc")]
    let mut seg = match sink {
        Some(_) if !plan.is_none() => Some(Segmenter::new(plan, *written)),
        _ => None,
    };
    #[cfg(feature = "crc")]
    let publish = |seg: Option<Segmenter>| {
        if let (Some(sink), Some(seg)) = (sink, seg) {
            let checks = seg.finish();
            if checks.len != 0
                && let Ok(mut v) = sink.lock()
            {
                v.push(checks);
            }
        }
    };
    let mut src = StSource {
        replay,
        stream,
        buf: Vec::new(),
        cur: None,
        pos: 0,
        lim: 0,
        read_was_finished,
    };

    let mut wr_pos = 0usize;

    loop {
        src.fill()?;

        let dic_pos = dec.dic_pos();
        let size = {
            let mut next = dec.dic_buf_size();
            if next - wr_pos > OUT_STEP_ST {
                next = wr_pos + OUT_STEP_ST;
            }
            next - dic_pos
        };

        let (in_processed, status) = dec
            .decode_block(dic_pos + size, src.slice(), FinishMode::Any)
            .map_err(to_io)?;

        src.pos += in_processed;
        let out_processed = dec.dic_pos() - dic_pos;
        *written += out_processed as u64;

        let need_stop =
            (in_processed == 0 && out_processed == 0) || status == Status::FinishedWithMark;

        if need_stop || out_processed >= size {
            let end = dec.dic_pos();
            #[cfg(feature = "crc")]
            if let Some(seg) = seg.as_mut() {
                seg.update(dec.dic_slice(wr_pos, end));
            }
            out.write_all(dec.dic_slice(wr_pos, end))?;
            dec.wrap_dic_pos();
            wr_pos = dec.dic_pos();

            if need_stop {
                if status == Status::FinishedWithMark {
                    #[cfg(feature = "crc")]
                    publish(seg);
                    return Ok(*written);
                }
                if status == Status::NeedsMoreInput {
                    return Err(to_io(Error::Truncated));
                }
                return Err(to_io(Error::CorruptData));
            }
        }
    }
}

/// The single-threaded decoder, its dictionary lent when the backing covers
/// it, exactly as [`crate::Lzma2Reader::with_window`] builds it, with its
/// errors.
fn tail_decoder(dict_prop: u8, window: Option<&WindowBacking>) -> io::Result<Lzma2Decoder> {
    let prop = Lzma2Decoder::capped_props(dict_prop, u32::MAX).map_err(to_io)?;
    match crate::window::lend(window, prop.dic_buf_size())? {
        Some(mem) => Lzma2Decoder::with_caller_dic(prop, mem),
        None => Lzma2Decoder::new(dict_prop),
    }
    .map_err(to_io)
}

fn to_io(e: Error) -> io::Error {
    e.into()
}

// ---------------------------------------------------------------------------
// Read adapter
// ---------------------------------------------------------------------------

/// Block of decoded output on its way from the decoding threads to the reader.
type Block = io::Result<Vec<u8>>;

/// The most bytes of output one piece on the reader's channel holds.
const READER_PIECE: usize = OUT_STEP_ST;

/// The pieces the reader and its coordinator can hold at once: the one the
/// reader is reading, the one on the channel, the one the coordinator is
/// sending, and the two spares on their way back.
const READER_PIECES: u64 = 5;

/// The `Write` end of the channel the reader pulls from. A block is copied
/// across in pieces of at most [`READER_PIECE`] bytes, so the reader never
/// holds a block-sized copy, whatever the block's size or where it lives;
/// [`Lzma2ParallelReader`] hands the pieces back for reuse.
struct ChannelSink {
    tx: SyncSender<Block>,
    spare: Receiver<Vec<u8>>,
}

impl Write for ChannelSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let buf = &buf[..buf.len().min(READER_PIECE)];
        let mut v = match self.spare.try_recv() {
            Ok(mut v) => {
                v.clear();
                v
            }
            Err(_) => Vec::with_capacity(buf.len()),
        };
        v.extend_from_slice(buf);
        self.tx
            .send(Ok(v))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "reader went away"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Streaming multi-threaded LZMA2 decoder over any [`Read`].
///
/// The ring of threads runs behind the reader and hands its output across in
/// pieces of at most 1 MiB; dropping the reader stops them and joins them.
/// [`Lzma2ParallelReader::memory_estimate`] bounds what it holds in RAM.
///
/// # Why `R: Read + Send + 'static`
///
/// The reader is moved onto the coordinating thread, and that thread outlives
/// every call: this type *is* a `Read`, so the caller keeps it and reads from
/// it later. A scoped thread cannot be used for that, since a scope ends
/// before the function that opened it returns, so the input cannot be a borrow of
/// something on the caller's stack, which is what a bounded view into an
/// archive usually is.
///
/// Two ways around it, both already here:
///
/// - [`Lzma2ParallelDecoder::decode`] takes `R: Read + Send` with no `'static`
///   bound. It owns its threads for the length of one call and joins them
///   before returning, so a borrowed source is sound. Use it when the output
///   can go to a [`Write`] rather than being pulled.
/// - [`Lzma2AdaptiveDecoder`](crate::Lzma2AdaptiveDecoder) borrows nothing at
///   all: the caller feeds it slices and it hands workers owned copies of
///   complete runs. Use it when the output has to be pulled, or when the
///   thread count has to change while the stream is being decoded.
pub struct Lzma2ParallelReader<R: Read + Send + 'static> {
    rx: Receiver<Block>,
    #[cfg(feature = "crc")]
    checks: Option<ChecksumSink>,
    spare_tx: SyncSender<Vec<u8>>,
    join: Option<std::thread::JoinHandle<io::Result<u64>>>,
    cur: Vec<u8>,
    pos: usize,
    done: bool,
    estimate: u64,
    _marker: core::marker::PhantomData<R>,
}

impl<R: Read + Send + 'static> Lzma2ParallelReader<R> {
    /// Builds a reader over a raw LZMA2 stream.
    ///
    /// # Errors
    ///
    /// Fails if the property byte is out of range or the coordinating thread
    /// cannot be spawned.
    pub fn new(inner: R, dict_prop: u8, options: &Lzma2MtOptions) -> io::Result<Self> {
        let dec = Lzma2ParallelDecoder::new(dict_prop, options).map_err(to_io)?;
        #[cfg(feature = "crc")]
        return Self::build(inner, dec, &ChecksumPlan::none());
        #[cfg(not(feature = "crc"))]
        return Self::build(inner, dec);
    }

    /// As [`Lzma2ParallelReader::new`], with every dictionary buffer over the
    /// window's in-RAM limit on the window's allocator, as
    /// [`Lzma2ParallelDecoder::with_window`] describes.
    ///
    /// # Errors
    ///
    /// As [`Lzma2ParallelReader::new`]. A window the decode cannot get fails
    /// the read that needed it, with the error
    /// [`Lzma2ParallelDecoder::with_window`] describes.
    pub fn with_window(
        inner: R,
        dict_prop: u8,
        options: &Lzma2MtOptions,
        window: WindowBacking,
    ) -> io::Result<Self> {
        let dec = Lzma2ParallelDecoder::with_window(dict_prop, options, window).map_err(to_io)?;
        #[cfg(feature = "crc")]
        return Self::build(inner, dec, &ChecksumPlan::none());
        #[cfg(not(feature = "crc"))]
        return Self::build(inner, dec);
    }

    /// Builds a reader that also checksums each block in the worker that
    /// produced it. Drain what has been computed so far with
    /// [`Lzma2ParallelReader::take_checks`].
    ///
    /// # Errors
    ///
    /// As [`Lzma2ParallelReader::new`].
    #[cfg(feature = "crc")]
    pub fn with_checksums(
        inner: R,
        dict_prop: u8,
        options: &Lzma2MtOptions,
        plan: &ChecksumPlan,
    ) -> io::Result<Self> {
        let dec = Lzma2ParallelDecoder::new(dict_prop, options).map_err(to_io)?;
        Self::build(inner, dec, plan)
    }

    /// [`Lzma2ParallelReader::with_checksums`] and
    /// [`Lzma2ParallelReader::with_window`] together.
    ///
    /// # Errors
    ///
    /// As [`Lzma2ParallelReader::with_window`].
    #[cfg(feature = "crc")]
    pub fn with_checksums_and_window(
        inner: R,
        dict_prop: u8,
        options: &Lzma2MtOptions,
        plan: &ChecksumPlan,
        window: WindowBacking,
    ) -> io::Result<Self> {
        let dec = Lzma2ParallelDecoder::with_window(dict_prop, options, window).map_err(to_io)?;
        Self::build(inner, dec, plan)
    }

    /// An upper bound on the bytes the reader and the decode behind it hold
    /// in RAM: [`Lzma2ParallelDecoder::memory_estimate`] for its options and
    /// window, plus the 5 MiB of output pieces on their way to the caller.
    /// Memory lent through a [`WindowBacking`] is not counted.
    #[must_use]
    pub fn memory_estimate(&self) -> u64 {
        self.estimate
    }

    /// Everything checksummed since the last call, in output order.
    ///
    /// Blocks appear here as soon as the worker that produced them reaches the
    /// write section, which is generally *before* the reader has been asked
    /// for those bytes. After the reader has returned `Ok(0)` this holds
    /// everything the decode produced.
    #[cfg(feature = "crc")]
    pub fn take_checks(&mut self) -> Vec<BlockChecks> {
        match self.checks.as_ref().and_then(|s| s.lock().ok()) {
            Some(mut v) => core::mem::take(&mut *v),
            None => Vec::new(),
        }
    }

    /// Everything checksummed since the last call, flattened to segments.
    ///
    /// The in-order convenience over [`Lzma2ParallelReader::take_checks`] for
    /// a consumer that only wants the CRCs.
    #[cfg(feature = "crc")]
    pub fn take_segments(&mut self) -> Vec<checksum::Segment> {
        self.take_checks()
            .into_iter()
            .flat_map(|c| c.segments)
            .collect()
    }

    fn build(
        inner: R,
        dec: Lzma2ParallelDecoder,
        #[cfg(feature = "crc")] plan: &ChecksumPlan,
    ) -> io::Result<Self> {
        // One block in flight on the channel, plus the one the reader holds:
        // the same bound the ring itself uses, one unit of lookahead.
        let (tx, rx) = sync_channel::<Block>(1);
        let (spare_tx, spare) = sync_channel::<Vec<u8>>(2);
        let estimate = dec
            .memory_estimate()
            .saturating_add(READER_PIECES * READER_PIECE as u64);
        let err_tx = tx.clone();
        #[cfg(feature = "crc")]
        let checks: Option<ChecksumSink> = if plan.is_none() {
            None
        } else {
            Some(Arc::new(Mutex::new(Vec::new())))
        };
        #[cfg(feature = "crc")]
        let (thread_checks, thread_plan) = (checks.clone(), plan.clone());
        let join = std::thread::Builder::new()
            .name("lzma2-mt-reader".into())
            .spawn(move || {
                let mut sink = ChannelSink { tx, spare };
                #[cfg(feature = "crc")]
                let r = match thread_checks.as_ref() {
                    Some(c) => dec.decode_to_sink(inner, &mut sink, &thread_plan, c),
                    None => dec.decode(inner, &mut sink),
                };
                #[cfg(not(feature = "crc"))]
                let r = dec.decode(inner, &mut sink);
                // The error itself goes to the reader, so that it can be
                // downcast; only one that cannot be queued is left for `finish`.
                match r {
                    Err(e) => match err_tx.try_send(Err(e)) {
                        Ok(()) => Ok(0),
                        Err(TrySendError::Full(r) | TrySendError::Disconnected(r)) => r.map(|_| 0),
                    },
                    ok => ok,
                }
            })
            .map_err(|_| to_io(Error::Alloc))?;
        Ok(Lzma2ParallelReader {
            rx,
            #[cfg(feature = "crc")]
            checks,
            spare_tx,
            join: Some(join),
            cur: Vec::new(),
            pos: 0,
            done: false,
            estimate,
            _marker: core::marker::PhantomData,
        })
    }
}

impl<R: Read + Send + 'static> Read for Lzma2ParallelReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.pos == self.cur.len() {
            if self.done {
                return Ok(0);
            }
            if !self.cur.is_empty() {
                let done = core::mem::take(&mut self.cur);
                if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) =
                    self.spare_tx.try_send(done)
                {
                    // No room for a spare; drop it.
                }
            }
            self.pos = 0;
            match self.rx.recv() {
                Ok(Ok(v)) => self.cur = v,
                Ok(Err(e)) => {
                    self.done = true;
                    return Err(e);
                }
                Err(_) => {
                    self.done = true;
                    return self.finish().map(|()| 0);
                }
            }
        }
        let n = (self.cur.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl<R: Read + Send + 'static> Lzma2ParallelReader<R> {
    fn finish(&mut self) -> io::Result<()> {
        match self.join.take() {
            Some(h) => match h.join() {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(to_io(Error::InternalFailure)),
            },
            None => Ok(()),
        }
    }
}

impl<R: Read + Send + 'static> Drop for Lzma2ParallelReader<R> {
    fn drop(&mut self) {
        // Hang up so the coordinator's next write fails, then join it: no
        // thread outlives the reader.
        self.done = true;
        let (tx, rx) = sync_channel::<Block>(0);
        drop(tx);
        let rx = core::mem::replace(&mut self.rx, rx);
        drop(rx);
        let _ = self.finish();
    }
}
