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
    per_thread: u64,
    st_only: bool,
}

fn plan(dict_prop: u8, options: &Lzma2MtOptions) -> Plan {
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

    let per_thread = expected + in_block_max + OVERHEAD_SIZE;
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
        out_block_max: expected as usize,
        per_thread,
        st_only: threads <= 1,
    }
}

/// A prepared multi-threaded LZMA2 decode.
///
/// Holds no decoder state: it is the plan, and each call to
/// [`Lzma2ParallelDecoder::decode`] runs a ring of threads that exists only
/// for the duration of that call.
#[derive(Debug, Clone, Copy)]
pub struct Lzma2ParallelDecoder {
    dict_prop: u8,
    plan: Plan,
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
            plan: plan(dict_prop, options),
        })
    }

    /// How many threads the plan will use. One means the single-threaded
    /// decoder runs directly, with no threads created and no overhead.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.plan.threads
    }

    /// An upper bound on the bytes this decode will allocate.
    #[must_use]
    pub fn memory_estimate(&self) -> u64 {
        memory_estimate(self.dict_prop, self.plan.threads, self.plan.per_thread)
    }

    /// Decodes `input` to `out` and returns the number of bytes written.
    ///
    /// C: `Lzma2DecMt_Decode`.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from either stream, and returns
    /// [`std::io::ErrorKind::InvalidData`] wrapping an [`Error`] for a stream
    /// that does not decode.
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
                // an end marker, so there was no end marker.
                return Err(to_io(Error::CorruptData));
            }
            return Ok(written);
        }

        // C: `MtDec_Code` returned with `needContinue`, so the stream is
        // finished single-threaded, starting from the input the threaded pass
        // read but did not decode.
        decode_st(
            self.dict_prop,
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

/// An upper bound on the memory a decode with this plan will allocate.
fn memory_estimate(dict_prop: u8, threads: usize, per_thread: u64) -> u64 {
    let dict_size = u64::from(dic_size_from_prop_full(dict_prop));
    // The single-threaded tail can run after the threaded pass, and its
    // dictionary and input buffer are additional to whatever the threaded pass
    // is still holding for replay.
    let st = dict_size + IN_BUF_SIZE_ST as u64;
    if threads <= 1 {
        st
    } else {
        threads as u64 * per_thread + st
    }
}

/// An upper bound on the memory [`Lzma2ParallelDecoder`] would allocate for
/// these options, without building one.
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
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
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
    stream: &mut (dyn Read + Send),
    out: &mut (dyn Write + Send),
    replay: VecDeque<ReplayBuf>,
    read_was_finished: bool,
    written: &mut u64,
    #[cfg(feature = "crc")] plan: &ChecksumPlan,
    #[cfg(feature = "crc")] sink: Option<&ChecksumSink>,
) -> io::Result<u64> {
    let mut dec = Lzma2Decoder::new(dict_prop).map_err(to_io)?;

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
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated LZMA2 stream",
                    ));
                }
                return Err(to_io(Error::CorruptData));
            }
        }
    }
}

fn to_io(e: Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

// ---------------------------------------------------------------------------
// Read adapter
// ---------------------------------------------------------------------------

/// Block of decoded output on its way from the decoding threads to the reader.
type Block = io::Result<Vec<u8>>;

/// The `Write` end of the channel the reader pulls from. Blocks are moved, not
/// copied: [`Lzma2ParallelReader`] hands the buffers back for reuse.
struct ChannelSink {
    tx: SyncSender<Block>,
    spare: Receiver<Vec<u8>>,
}

impl Write for ChannelSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut v = match self.spare.try_recv() {
            Ok(mut v) => {
                v.clear();
                v
            }
            Err(_) => Vec::new(),
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
/// The ring of threads runs behind the reader and hands whole blocks across;
/// dropping the reader stops them and joins them.
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
        #[cfg(feature = "crc")]
        return Self::build(inner, dict_prop, options, &ChecksumPlan::none());
        #[cfg(not(feature = "crc"))]
        return Self::build(inner, dict_prop, options);
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
        Self::build(inner, dict_prop, options, plan)
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
        dict_prop: u8,
        options: &Lzma2MtOptions,
        #[cfg(feature = "crc")] plan: &ChecksumPlan,
    ) -> io::Result<Self> {
        let dec = Lzma2ParallelDecoder::new(dict_prop, options).map_err(to_io)?;
        // One block in flight on the channel, plus the one the reader holds:
        // the same bound the ring itself uses, one unit of lookahead.
        let (tx, rx) = sync_channel::<Block>(1);
        let (spare_tx, spare) = sync_channel::<Vec<u8>>(2);
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
                if let Err(e) = &r {
                    let _ = err_tx.try_send(Err(io::Error::new(e.kind(), e.to_string())));
                }
                r
            })?;
        Ok(Lzma2ParallelReader {
            rx,
            #[cfg(feature = "crc")]
            checks,
            spare_tx,
            join: Some(join),
            cur: Vec::new(),
            pos: 0,
            done: false,
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
                Err(_) => Err(io::Error::other("LZMA2 decoding thread panicked")),
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
