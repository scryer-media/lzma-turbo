//! Decoding an `.xz` file that is still arriving.
//!
//! C: nothing. 7-Zip's `XzDecMt` is handed a file; this is the shape a caller
//! chasing a download needs, and it is the same shape
//! [`crate::Lzma2AdaptiveDecoder`] has, for the same reasons: input is fed
//! rather than read, output is polled as `(offset, bytes)`, nothing blocks on
//! bytes that have not arrived, and the thread count can be changed
//! mid-stream.
//!
//! What makes an xz file easier to chase than a raw LZMA2 stream is that its
//! blocks announce themselves. A block header that declares both sizes says
//! exactly where the block ends *and* how much it decodes to, so as soon as
//! those bytes have arrived the whole block can go to a worker without
//! decoding anything first. That is the multi-threaded mode. A block header
//! that declares no compressed size - which `xz` only writes for a
//! single-block stream - cannot be skipped over, so it is decoded on the
//! caller's thread as the bytes arrive. That is the chase, and it is also
//! what happens at the tail of every file, where the last block is still
//! being written.
//!
//! The two modes are ordered by construction: input is consumed strictly in
//! file order, so a block handed to a worker always precedes the block being
//! chased, and the chase only runs when nothing is outstanding. The caller
//! therefore sees output in file order, with offsets it can write by.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::XzOptions;
use super::block::{BlockHeader, MAX_BLOCK_HEADER_SIZE, header_size_from_first_byte};
use super::blockdec::{BlockDecoder, BlockLimits};
use super::check::BlockCheck;
use super::error::{XzError, XzErrorKind, XzResult};
use super::index::{IndexFold, MAX_INDEX_SIZE};
use super::pool::{XzDone, XzJob, XzPool};
use super::stream::{
    CheckType, STREAM_FOOTER_SIZE, STREAM_HEADER_SIZE, StreamFlags, StreamFooter, StreamHeader,
};
use super::vli;
use crate::buffer::DicBuf;
use crate::crc::Crc32;
use crate::mt::adaptive::DrainStatus;

/// How much output the chase decoder produces per turn before handing control
/// back. C: `props.outStep_ST`.
const OUT_STEP: usize = 1 << 20;

/// Where the decoder is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    StreamHeader,
    BlockOrIndex,
    Block,
    Index,
    Footer,
    Padding,
    Done,
}

/// An `.xz` decoder that is fed input and decides, block by block, whether to
/// chase the tail on the caller's thread or hand a finished block to a worker.
pub struct XzAdaptiveDecoder {
    opts: XzOptions,
    /// Bytes fed and not yet consumed. `base` is the file offset of `buf[0]`.
    buf: Vec<u8>,
    base: u64,
    cursor: usize,
    state: State,
    stream: u64,
    block_no: u64,
    flags: StreamFlags,
    check_size: usize,
    fold: IndexFold,
    /// The fold closed, once, when the index indicator was reached.
    fold_final: (u64, u64, u64, u64),
    index_size: u64,
    /// How far the index has been parsed, kept across calls so that an
    /// index arriving in pieces is read once, not again from its start on
    /// every call that brings more of it.
    index_parse: IndexParse,

    // The chase.
    st: Option<BlockDecoder>,
    st_header_size: usize,
    st_out: Vec<u8>,

    // The workers.
    pool: Option<XzPool>,
    threads: usize,
    outstanding: usize,
    outstanding_bytes: u64,
    dispatched: u64,
    /// Where the next block's output will land: `emitted` plus everything
    /// already handed to workers. Blocks are dispatched in file order, so this
    /// is exact.
    planned_out: u64,
    next_emit: u64,
    ready: BTreeMap<u64, XzDone>,
    /// The block a limited drain stopped in the middle of: its buffer, how
    /// much of it is output, and how much of that has been handed over.
    part: Option<(DicBuf, usize, usize)>,
    spare_out: Vec<Vec<u8>>,
    spare_in: Vec<Vec<u8>>,

    emitted: u64,
    checks: Vec<BlockCheck>,
    input_done: bool,
    cancelled: bool,
    /// The first error `feed` or a drain returned. Every later call returns
    /// it again without touching the state machine, which that error may
    /// have left part way through a step.
    failed: Option<XzError>,
    /// The allocator's own failure behind the last
    /// [`XzErrorKind::WindowAlloc`] returned, for
    /// [`XzAdaptiveDecoder::take_window_error`]. `AssertUnwindSafe` keeps the
    /// decoder's auto traits, as an `io::Error` would not.
    window_err: core::panic::AssertUnwindSafe<Option<crate::WindowAllocError>>,
}

impl XzAdaptiveDecoder {
    /// A decoder with the given options. `XzOptions::threads` is the ceiling,
    /// not a promise: workers are created only when there is a finished block
    /// for one.
    #[must_use]
    pub fn new(opts: XzOptions) -> Self {
        let threads = if opts.threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            opts.threads
        };
        XzAdaptiveDecoder {
            opts,
            buf: Vec::new(),
            base: 0,
            cursor: 0,
            state: State::StreamHeader,
            stream: 0,
            block_no: 0,
            flags: StreamFlags {
                check: CheckType::None,
                raw: [0, 0],
            },
            check_size: 0,
            fold: IndexFold::default(),
            fold_final: (0, 0, 0, 0),
            index_size: 0,
            index_parse: IndexParse::new(),
            st: None,
            st_header_size: 0,
            st_out: Vec::new(),
            pool: None,
            threads,
            outstanding: 0,
            outstanding_bytes: 0,
            dispatched: 0,
            planned_out: 0,
            next_emit: 0,
            ready: BTreeMap::new(),
            part: None,
            spare_out: Vec::new(),
            spare_in: Vec::new(),
            emitted: 0,
            checks: Vec::new(),
            input_done: false,
            cancelled: false,
            failed: None,
            window_err: core::panic::AssertUnwindSafe(None),
        }
    }

    /// Changes the thread ceiling. Takes effect at the next block: a block
    /// already with a worker is left alone.
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads.max(1);
    }

    /// The current thread ceiling.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// How many worker threads have actually been created.
    #[must_use]
    pub fn spawned_threads(&self) -> usize {
        self.pool.as_ref().map_or(0, XzPool::spawned)
    }

    /// Bytes awaiting consumption or delivery: buffered input, worker jobs,
    /// completed blocks awaiting order, and partially delivered output.
    #[must_use]
    pub fn in_flight_bytes(&self) -> u64 {
        ((self.buf.len() - self.cursor) as u64).saturating_add(self.held_block_bytes())
    }

    fn held_block_bytes(&self) -> u64 {
        // Only bytes in RAM count: a block on caller memory holds its packed
        // bytes alone.
        let ready = self
            .ready
            .values()
            .fold(self.outstanding_bytes, |held, done| {
                held.saturating_add(in_ram(done.lent, done.unpacked_len))
                    .saturating_add(done.bytes.len() as u64)
            });
        // The whole output allocation remains owned until its final byte is
        // delivered, even when only a small suffix is left.
        ready.saturating_add(
            self.part
                .as_ref()
                .map_or(0, |(buf, len, _)| in_ram(buf.is_lent(), *len)),
        )
    }

    /// The allocator's own failure behind the [`XzErrorKind::WindowAlloc`]
    /// the decoder last returned, once, with its location as
    /// [`crate::WindowAllocError::xz_error`]; `None` when no allocator has
    /// failed it, or when it has been taken.
    pub fn take_window_error(&mut self) -> Option<crate::WindowAllocError> {
        self.window_err.0.take()
    }

    /// Keeps the allocator's failure behind `error` for
    /// [`XzAdaptiveDecoder::take_window_error`], and returns `error`.
    fn window_failed(
        &mut self,
        error: XzError,
        window: Option<crate::WindowAllocError>,
    ) -> XzError {
        self.window_err.0 = window
            .filter(|_| matches!(error.kind, XzErrorKind::WindowAlloc { .. }))
            .map(|w| w.located(error));
        error
    }

    /// Total output handed to the caller so far.
    #[must_use]
    pub fn total_out(&self) -> u64 {
        self.emitted
    }

    /// The per-block checks computed so far, in block order.
    #[must_use]
    pub fn take_checks(&mut self) -> Vec<BlockCheck> {
        core::mem::take(&mut self.checks)
    }

    /// Adds `data` to the stream and returns how much of it was taken.
    ///
    /// Never blocks and never decodes. Takes less than all of `data` only when
    /// the memory limit is reached, in which case the caller should drain and
    /// feed the rest.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::Lzma`] carrying `Cancelled` after
    /// [`XzAdaptiveDecoder::cancel`].
    pub fn feed(&mut self, data: &[u8]) -> XzResult<usize> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let fed = self.feed_impl(data);
        self.latch(fed)
    }

    /// Keeps the first error returned, for every later call to return.
    fn latch<T>(&mut self, res: XzResult<T>) -> XzResult<T> {
        if let Err(error) = &res {
            self.failed.get_or_insert(*error);
        }
        res
    }

    fn feed_impl(&mut self, data: &[u8]) -> XzResult<usize> {
        if self.cancelled {
            return Err(self.err(XzErrorKind::Lzma(crate::Error::Cancelled)));
        }
        if data.is_empty() || self.state == State::Done {
            return Ok(0);
        }
        let room = self
            .opts
            .memory_limit
            .saturating_sub(self.in_flight_bytes());
        let take = usize::try_from(room).unwrap_or(usize::MAX).min(data.len());
        // A limit smaller than one buffer would otherwise deadlock; the chase
        // streams, so progress never needs a large buffer.
        let take = if take == 0 && self.buf.len() == self.cursor {
            data.len().min(1 << 16)
        } else {
            take
        };
        self.compact();
        self.buf.extend_from_slice(&data[..take]);
        Ok(take)
    }

    /// Declares that no more input is coming. A file that then does not end
    /// properly is reported as truncated rather than waited on.
    pub fn end_of_input(&mut self) {
        self.input_done = true;
    }

    /// Stops the decode and joins the workers.
    pub fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(p) = self.pool.as_mut() {
            p.shutdown();
        }
        self.pool = None;
        self.outstanding = 0;
        self.outstanding_bytes = 0;
    }

    /// Blocks until a worker hands back a finished block, and takes it in.
    ///
    /// Returns false at once when no block is outstanding.
    ///
    /// [`drain`](XzAdaptiveDecoder::drain) hands control back as soon as there
    /// is nothing it can do without more input, even with workers still
    /// decoding, so that the caller can feed the next block rather than wait
    /// for the last one. A caller whose read-ahead is already satisfied has
    /// nothing to feed and nothing else to do; without somewhere to wait it
    /// would call drain again, and again, for as long as the workers took.
    /// This is that place: one block, no poll interval, no deadline.
    ///
    /// Everything else a worker has already finished is taken in along with
    /// the block waited for. A block that failed is not reported here - what it
    /// failed with is held until its turn to be handed over comes, and comes
    /// out of the next drain exactly as it would have without this call.
    ///
    /// False also comes back when the outstanding block can no longer arrive:
    /// the decode was cancelled, or every worker has gone. A caller looping on
    /// this therefore cannot spin - the next drain turns that state into the
    /// error it is.
    pub fn wait_for_worker(&mut self) -> bool {
        if self.outstanding == 0 || self.failed.is_some() {
            return false;
        }
        self.collect(true).unwrap_or(false)
    }

    /// Decodes as much as the bytes fed so far allow, handing each piece of
    /// output to `sink` as `(file offset in the decoded stream, bytes)`.
    ///
    /// Returns when nothing more can be done without input, when the file is
    /// finished, or when output was produced and the caller might want to
    /// reconsider the thread count.
    ///
    /// # Errors
    ///
    /// Any structural or check failure, located; a block that fails is
    /// reported only after every block before it has been handed over, so a
    /// caller writing by offset keeps what it has already written.
    pub fn drain<F>(&mut self, sink: F) -> XzResult<DrainStatus>
    where
        F: FnMut(u64, &[u8]),
    {
        self.drain_impl(usize::MAX, sink)
    }

    /// As [`drain`](XzAdaptiveDecoder::drain), but stops once `sink` has been
    /// handed `limit` bytes, keeping the rest of the block it was in the
    /// middle of for the next call.
    ///
    /// A block of a parallel-encoded `.xz` file is whatever the encoder chose,
    /// often tens or hundreds of megabytes, while a caller implementing `Read` is
    /// asked for what fits in a buffer. Without a limit the difference has to
    /// be spilled somewhere; with one, the decoder's memory is a function of
    /// what is in flight and not of what has been fed.
    ///
    /// A `limit` of zero delivers nothing. Ordering is unaffected: the part
    /// left over is the first thing the next call hands out.
    ///
    /// # Errors
    ///
    /// As [`drain`](XzAdaptiveDecoder::drain).
    pub fn drain_upto<F>(&mut self, limit: usize, sink: F) -> XzResult<DrainStatus>
    where
        F: FnMut(u64, &[u8]),
    {
        self.drain_impl(limit, sink)
    }

    fn drain_impl<F>(&mut self, limit: usize, sink: F) -> XzResult<DrainStatus>
    where
        F: FnMut(u64, &[u8]),
    {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let drained = self.drain_steps(limit, sink);
        self.latch(drained)
    }

    fn drain_steps<F>(&mut self, limit: usize, mut sink: F) -> XzResult<DrainStatus>
    where
        F: FnMut(u64, &[u8]),
    {
        if self.cancelled {
            return Err(self.err(XzErrorKind::Lzma(crate::Error::Cancelled)));
        }
        let mut left = limit;
        let mut progress = false;
        loop {
            let mut did = self.collect(false)?;
            did |= self.emit(&mut sink, &mut left)?;

            if self.state == State::Done
                && self.outstanding == 0
                && self.ready.is_empty()
                && self.part.is_none()
            {
                return Ok(DrainStatus::Finished);
            }
            if left == 0 {
                // The caller's buffer is full. The next call starts with the
                // part of the block that did not fit.
                return Ok(DrainStatus::Progress);
            }

            match self.step(&mut sink, &mut left)? {
                Step::Did => did = true,
                Step::Blocked => {
                    // Nothing can be done with what has arrived. If a worker
                    // is busy, wait for it rather than spinning - unless the
                    // caller could be feeding the next block to the next
                    // worker instead, which is the case whenever input is
                    // still coming and there is room to hold it.
                    if self.outstanding > 0
                        && !self.input_done
                        && self.in_flight_bytes() < self.opts.memory_limit
                    {
                        progress |= did;
                        return Ok(if progress {
                            DrainStatus::Progress
                        } else {
                            DrainStatus::NeedsMoreInput
                        });
                    }
                    if self.outstanding > 0 {
                        if self.collect(true)? {
                            did = true;
                        } else {
                            // A block was handed to a worker and no worker can
                            // return it. Nothing else will ever make progress,
                            // so this is reported rather than waited on.
                            return Err(self.err(XzErrorKind::Lzma(crate::Error::InternalFailure)));
                        }
                        did |= self.emit(&mut sink, &mut left)?;
                    } else {
                        progress |= did;
                        return Ok(if progress {
                            DrainStatus::Progress
                        } else {
                            DrainStatus::NeedsMoreInput
                        });
                    }
                }
            }
            progress |= did;
            if did && self.outstanding == 0 && self.ready.is_empty() {
                // Give the caller a chance to change the thread count between
                // blocks.
                return Ok(DrainStatus::Progress);
            }
        }
    }

    // -- the state machine -------------------------------------------------

    fn step<F>(&mut self, sink: &mut F, left: &mut usize) -> XzResult<Step>
    where
        F: FnMut(u64, &[u8]),
    {
        match self.state {
            State::Done => Ok(Step::Blocked),
            State::StreamHeader => self.stream_header(),
            State::BlockOrIndex => self.block_or_index(),
            State::Block => self.chase(sink, left),
            State::Index => self.index(),
            State::Footer => self.footer(),
            State::Padding => self.padding(),
        }
    }

    fn avail(&self) -> &[u8] {
        &self.buf[self.cursor..]
    }

    fn offset(&self) -> u64 {
        self.base + self.cursor as u64
    }

    fn consume(&mut self, n: usize) {
        self.cursor += n;
    }

    /// Drops consumed input once it is worth the copy.
    fn compact(&mut self) {
        if self.cursor > (1 << 20) || (self.cursor > 0 && self.cursor == self.buf.len()) {
            self.buf.drain(..self.cursor);
            self.base += self.cursor as u64;
            self.cursor = 0;
        }
    }

    fn err(&self, kind: XzErrorKind) -> XzError {
        match self.state {
            State::Block => XzError::in_block(kind, self.stream, self.block_no, self.offset()),
            _ => XzError::at(kind, self.stream, self.offset()),
        }
    }

    /// Input ended where a field was still expected.
    fn short(&self) -> XzResult<Step> {
        if self.input_done {
            Err(self.err(XzErrorKind::TruncatedInput))
        } else {
            Ok(Step::Blocked)
        }
    }

    fn stream_header(&mut self) -> XzResult<Step> {
        if self.avail().len() < STREAM_HEADER_SIZE {
            return self.short();
        }
        let mut raw = [0u8; STREAM_HEADER_SIZE];
        raw.copy_from_slice(&self.avail()[..STREAM_HEADER_SIZE]);
        let header = StreamHeader::parse(&raw).map_err(|k| self.err(k))?;
        let check = header.flags.check;
        if self.opts.verify_checks && !check.is_verifiable() && !self.opts.allow_unverifiable {
            return Err(self.err(XzErrorKind::UnsupportedCheck));
        }
        self.flags = header.flags;
        self.check_size = check.size();
        self.consume(STREAM_HEADER_SIZE);
        self.block_no = 0;
        self.fold = IndexFold::default();
        self.fold_final = (0, 0, 0, 0);
        self.state = State::BlockOrIndex;
        Ok(Step::Did)
    }

    fn block_or_index(&mut self) -> XzResult<Step> {
        let Some(&first) = self.avail().first() else {
            return self.short();
        };
        let Some(header_size) = header_size_from_first_byte(first) else {
            self.consume(1);
            // The fold is a running CRC, so it is closed exactly once, here,
            // and kept: the index may have to be parsed more than once as it
            // arrives.
            self.fold_final = self.fold.finish();
            self.state = State::Index;
            self.index_parse = IndexParse::new();
            return Ok(Step::Did);
        };
        if self.avail().len() < header_size {
            return self.short();
        }
        debug_assert!(header_size <= MAX_BLOCK_HEADER_SIZE);
        let header = BlockHeader::parse(&self.avail()[..header_size]).map_err(|k| self.err(k))?;

        // The multi-threaded case: a block that says where it ends and how big
        // it is can go to a worker whole, without being decoded first.
        if let (Some(packed), Some(unpacked)) = (header.compressed_size, header.uncompressed_size)
            && self.threads > 1
        {
            match self.try_dispatch(header_size, packed, unpacked)? {
                Dispatch::Sent => return Ok(Step::Did),
                Dispatch::Busy => return Ok(Step::Blocked),
                Dispatch::No => {}
            }
        }

        // The chase: decode it here, as it arrives. Only safe once everything
        // before it has been handed over, since this emits directly.
        if self.outstanding > 0 || !self.ready.is_empty() {
            return Ok(Step::Blocked);
        }
        if header
            .uncompressed_size
            .is_some_and(|u| u > self.block_cap())
        {
            return Err(self.err(XzErrorKind::TooMuchOutput {
                limit: self.block_cap(),
            }));
        }
        let limits = BlockLimits {
            memory_limit: self.opts.memory_limit,
            uncompressed_size: header.uncompressed_size,
            compressed_size: header.compressed_size,
            max_block_size: self.block_cap(),
        };
        let mut window_err = None;
        let dec = BlockDecoder::new(
            &header.chain,
            self.flags.check,
            self.emitted,
            &self.opts.plan,
            self.opts.verify_checks,
            limits,
            self.opts.window.as_ref(),
            &mut window_err,
        )
        .map_err(|k| {
            let error = self.err(k);
            self.window_failed(error, window_err.take())
        })?;
        self.st = Some(dec);
        self.st_header_size = header_size;
        self.consume(header_size);
        self.state = State::Block;
        Ok(Step::Did)
    }

    /// What one block may still produce, given the caller's per-block cap and
    /// its cap on the file.
    fn block_cap(&self) -> u64 {
        match self.opts.max_unpack_bytes {
            Some(cap) => self
                .opts
                .max_block_size
                .min(cap.saturating_sub(self.planned_out)),
            None => self.opts.max_block_size,
        }
    }

    /// Hands a fully-arrived block to a worker, if there is room for one.
    fn try_dispatch(
        &mut self,
        header_size: usize,
        packed: u64,
        unpacked: u64,
    ) -> XzResult<Dispatch> {
        if unpacked > self.block_cap() {
            return Err(self.err(XzErrorKind::TooMuchOutput {
                limit: self.block_cap(),
            }));
        }
        let padding = (4 - (packed % 4)) % 4;
        let unpadded = header_size as u64 + packed + self.check_size as u64;
        let padded = header_size as u64 + packed + padding + self.check_size as u64;
        let Ok(padded_usize) = usize::try_from(padded) else {
            return Err(self.err(XzErrorKind::SizeMismatch));
        };
        let Ok(unpacked_len) = usize::try_from(unpacked) else {
            return Err(self.err(XzErrorKind::SizeMismatch));
        };
        if self.avail().len() < padded_usize {
            // Not all of it has arrived. Waiting is right while there is still
            // room to buffer it; it is not right once the file is over (the
            // chase reports the truncation properly) or once the caller's
            // memory limit has been reached, because then no more input can be
            // accepted and waiting would be waiting forever. The chase decodes
            // as it reads, so it always makes progress.
            let starved = self.in_flight_bytes() >= self.opts.memory_limit;
            return Ok(if self.input_done || starved {
                Dispatch::No
            } else {
                Dispatch::Busy
            });
        }
        let held_blocks = self.outstanding + self.ready.len() + usize::from(self.part.is_some());
        if held_blocks >= self.threads {
            return Ok(Dispatch::Busy);
        }
        // A worker's block over the window's in-RAM limit goes on its own
        // window, asked for by the worker; one over the ceiling is refused
        // here and the allocator is never asked.
        let window = self
            .opts
            .window
            .as_ref()
            .filter(|w| w.covers(unpacked_len))
            .cloned();
        if let Some(w) = &window
            && !w.admits(unpacked_len)
        {
            return Err(self.err(XzErrorKind::MemoryLimit {
                needed: unpacked,
                limit: w.max_len(),
            }));
        }
        // One worker's cost in RAM, against the caller's limit. A block that
        // could not fit even alone is never a worker's: nothing finishing
        // would make room for it, so it is chased, which decodes in the
        // block's dictionary under the limit.
        let cost = in_ram(window.is_some(), unpacked_len).saturating_add(padded);
        if cost > self.opts.memory_limit {
            return Ok(Dispatch::No);
        }
        if held_blocks > 0 && self.held_block_bytes().saturating_add(cost) > self.opts.memory_limit
        {
            return Ok(Dispatch::Busy);
        }

        let mut bytes = self.spare_in.pop().unwrap_or_default();
        bytes.clear();
        // The block size is known; do not double a recycled packed buffer
        // when a later block exceeds its capacity by only a few bytes.
        bytes
            .try_reserve_exact(padded_usize)
            .map_err(|_| self.err(XzErrorKind::Lzma(crate::Error::Alloc)))?;
        bytes.extend_from_slice(&self.avail()[..padded_usize]);
        let file_offset = self.offset();
        let unpacked_offset = self.planned_out;
        let index = self.dispatched;
        let stream = self.stream;
        let block_in_stream = self.block_no;
        let check = self.flags.check;
        let verify = self.opts.verify_checks;
        let plan = self.opts.plan.clone();
        let out = if window.is_some() {
            DicBuf::default()
        } else {
            DicBuf::Owned(self.spare_out.pop().unwrap_or_default())
        };
        let threads = self.threads;
        let pool = self.pool.get_or_insert_with(XzPool::new);
        if pool.spawned() < threads {
            pool.grow();
        }
        let job = XzJob {
            index,
            stream,
            block_in_stream,
            file_offset,
            unpacked_offset,
            unpacked_len,
            unpadded_size: unpadded,
            bytes,
            check,
            verify,
            plan,
            out,
            window,
            window_err: None,
        };
        let sent = pool.dispatch(job);
        sent.map_err(|k| self.err(k))?;
        self.dispatched += 1;
        self.planned_out += unpacked;
        self.outstanding += 1;
        self.outstanding_bytes += cost;
        self.consume(padded_usize);
        self.fold
            .push(unpadded, unpacked)
            .map_err(|k| self.err(k))?;
        self.block_no += 1;
        self.state = State::BlockOrIndex;
        Ok(Dispatch::Sent)
    }

    /// One turn of the chase decoder.
    fn chase<F>(&mut self, sink: &mut F, left: &mut usize) -> XzResult<Step>
    where
        F: FnMut(u64, &[u8]),
    {
        // Never decode more than the caller has room for: bytes the chase
        // produces go straight to the sink, so anything over the limit would
        // have to be held here instead.
        let want = OUT_STEP.min(*left);
        if want == 0 {
            return Ok(Step::Blocked);
        }
        if self.st_out.len() < want {
            self.st_out.resize(want, 0u8);
        }
        let finished = {
            let dec = self.st.as_mut().expect("chasing");
            if dec.finished() {
                true
            } else {
                let (read, wrote) = dec
                    .decode(&self.buf[self.cursor..], &mut self.st_out[..want])
                    .map_err(|k| {
                        XzError::in_block(
                            k,
                            self.stream,
                            self.block_no,
                            self.base + self.cursor as u64,
                        )
                    })?;
                self.cursor += read;
                if wrote > 0 {
                    sink(self.emitted, &self.st_out[..wrote]);
                    self.emitted += wrote as u64;
                    *left -= wrote.min(*left);
                    self.planned_out = self.emitted;
                    if let Some(cap) = self.opts.max_unpack_bytes
                        && self.emitted > cap
                    {
                        return Err(self.err(XzErrorKind::TooMuchOutput { limit: cap }));
                    }
                    return Ok(Step::Did);
                }
                if read == 0 {
                    return self.short();
                }
                return Ok(Step::Did);
            }
        };
        debug_assert!(finished);

        // The block's data is done: its padding and check follow.
        let dec = self.st.as_ref().expect("chasing");
        let packed = dec.packed();
        let unpacked = dec.unpacked();
        let padding = usize::try_from((4 - (packed % 4)) % 4).expect("0..4");
        let need = padding + self.check_size;
        if self.avail().len() < need {
            return self.short();
        }
        let tail: Vec<u8> = self.avail()[..need].to_vec();
        if tail[..padding].iter().any(|&b| b != 0) {
            return Err(self.err(XzErrorKind::BadPadding));
        }
        self.consume(need);
        let dec = self.st.take().expect("chasing");
        let check = dec.finish(&tail[padding..]).map_err(|k| self.err(k))?;
        if !self.opts.plan.is_none() {
            self.checks.push(check);
        }
        let unpadded = self.st_header_size as u64 + packed + self.check_size as u64;
        self.fold
            .push(unpadded, unpacked)
            .map_err(|k| self.err(k))?;
        self.block_no += 1;
        self.state = State::BlockOrIndex;
        Ok(Step::Did)
    }

    /// Parses the index as it arrives. The indicator byte has already been
    /// consumed. The index is consumed only once it has been read whole, so
    /// every error is located at its start, however the input was split.
    fn index(&mut self) -> XzResult<Step> {
        let mut parse = core::mem::replace(&mut self.index_parse, IndexParse::new());
        let res = self.parse_index(&mut parse);
        let size = parse.size;
        let pos = parse.pos;
        self.index_parse = parse;
        if !res? {
            return self.incomplete_index();
        }
        self.consume(pos);
        self.index_size = size;
        self.state = State::Footer;
        Ok(Step::Did)
    }

    /// Carries the parse of the index on from where `p` stopped. Returns
    /// whether the index is complete; false means more of it is needed. A
    /// variable-length integer that has only partly arrived is left unread,
    /// and read again whole on the next call.
    fn parse_index(&self, p: &mut IndexParse) -> XzResult<bool> {
        let body = self.avail();
        // One integer at `p.pos`: `None` when it has not all arrived.
        let take_vli = |p: &mut IndexParse| -> XzResult<Option<u64>> {
            match vli::decode(body.get(p.pos..).unwrap_or(&[])) {
                Ok((v, n)) => {
                    p.hash(&body[p.pos..p.pos + n]);
                    p.pos += n;
                    p.size += n as u64;
                    Ok(Some(v))
                }
                Err(XzErrorKind::TruncatedVli) => Ok(None),
                Err(k) => Err(self.err(k)),
            }
        };
        loop {
            match p.phase {
                IndexPhase::Count => {
                    let Some(count) = take_vli(p)? else {
                        return Ok(false);
                    };
                    if count != self.fold_final.0 {
                        return Err(self.err(XzErrorKind::IndexMismatch));
                    }
                    p.records_left = count;
                    p.phase = IndexPhase::Records;
                }
                IndexPhase::Records => {
                    while p.records_left > 0 {
                        let unpadded = match p.unpadded {
                            Some(v) => v,
                            None => {
                                let Some(v) = take_vli(p)? else {
                                    return Ok(false);
                                };
                                p.unpadded = Some(v);
                                v
                            }
                        };
                        let Some(uncompressed) = take_vli(p)? else {
                            return Ok(false);
                        };
                        p.unpadded = None;
                        p.records_left -= 1;
                        #[cfg(test)]
                        {
                            p.records_parsed += 1;
                        }
                        if unpadded < 5 {
                            return Err(self.err(XzErrorKind::IndexMismatch));
                        }
                        p.seen
                            .push(unpadded, uncompressed)
                            .map_err(|k| self.err(k))?;
                        if p.size > MAX_INDEX_SIZE {
                            return Err(self.err(XzErrorKind::IndexMismatch));
                        }
                    }
                    let (_, have_blocks, have_unpacked, have_digest) = self.fold_final;
                    let (_, seen_blocks, seen_unpacked, seen_digest) = p.seen.finish();
                    if seen_digest != have_digest
                        || seen_blocks != have_blocks
                        || seen_unpacked != have_unpacked
                    {
                        return Err(self.err(XzErrorKind::IndexMismatch));
                    }
                    p.phase = IndexPhase::Padding;
                }
                IndexPhase::Padding => {
                    while !p.size.is_multiple_of(4) {
                        let Some(&b) = body.get(p.pos) else {
                            return Ok(false);
                        };
                        if b != 0 {
                            return Err(self.err(XzErrorKind::BadPadding));
                        }
                        p.hash(&[0]);
                        p.pos += 1;
                        p.size += 1;
                    }
                    p.phase = IndexPhase::Crc;
                }
                IndexPhase::Crc => {
                    let Some(stored) = body.get(p.pos..p.pos + 4) else {
                        return Ok(false);
                    };
                    let stored = u32::from_le_bytes([stored[0], stored[1], stored[2], stored[3]]);
                    if core::mem::take(&mut p.crc).finalize() != stored {
                        return Err(self.err(XzErrorKind::HeaderCrc));
                    }
                    p.pos += 4;
                    p.size += 4;
                    return Ok(true);
                }
            }
        }
    }

    /// The index has not all arrived yet.
    fn incomplete_index(&self) -> XzResult<Step> {
        if self.input_done {
            return Err(self.err(XzErrorKind::TruncatedInput));
        }
        // The index is the one field that has to be buffered whole, so it is
        // also the one that can ask for more than the caller allows. Saying so
        // is better than waiting for input `feed` will refuse to take.
        let held = self.in_flight_bytes();
        if held >= self.opts.memory_limit {
            return Err(self.err(XzErrorKind::MemoryLimit {
                needed: held + 1,
                limit: self.opts.memory_limit,
            }));
        }
        Ok(Step::Blocked)
    }

    fn footer(&mut self) -> XzResult<Step> {
        if self.avail().len() < STREAM_FOOTER_SIZE {
            return self.short();
        }
        let mut raw = [0u8; STREAM_FOOTER_SIZE];
        raw.copy_from_slice(&self.avail()[..STREAM_FOOTER_SIZE]);
        let footer = StreamFooter::parse(&raw).map_err(|k| self.err(k))?;
        if footer.flags.raw != self.flags.raw {
            return Err(self.err(XzErrorKind::StreamFlagsMismatch));
        }
        if footer.index_size != self.index_size {
            return Err(self.err(XzErrorKind::IndexMismatch));
        }
        self.consume(STREAM_FOOTER_SIZE);
        self.stream += 1;
        self.state = State::Padding;
        Ok(Step::Did)
    }

    fn padding(&mut self) -> XzResult<Step> {
        loop {
            if self.avail().is_empty() {
                if self.input_done {
                    self.state = State::Done;
                    return Ok(Step::Did);
                }
                return Ok(Step::Blocked);
            }
            if !self.opts.concatenated {
                return Err(self.err(XzErrorKind::TrailingGarbage));
            }
            if self.avail().len() < 4 {
                if self.input_done {
                    return Err(self.err(XzErrorKind::TrailingGarbage));
                }
                return Ok(Step::Blocked);
            }
            if self.avail()[..4] == [0, 0, 0, 0] {
                self.consume(4);
                continue;
            }
            self.state = State::StreamHeader;
            return Ok(Step::Did);
        }
    }

    // -- workers -----------------------------------------------------------

    /// Takes finished blocks off the pool. With `wait`, blocks until one
    /// arrives.
    fn collect(&mut self, wait: bool) -> XzResult<bool> {
        let mut did = false;
        loop {
            if self.outstanding == 0 {
                return Ok(did);
            }
            let Some(pool) = self.pool.as_ref() else {
                return Ok(did);
            };
            let done = if wait && !did {
                pool.collect()
            } else {
                pool.try_collect()
            };
            let Some(done) = done else {
                return Ok(did);
            };
            self.outstanding -= 1;
            self.outstanding_bytes = self
                .outstanding_bytes
                .saturating_sub(in_ram(done.lent, done.unpacked_len) + done.bytes.len() as u64);
            self.ready.insert(done.index, done);
            did = true;
        }
    }

    /// Hands finished blocks to the caller in file order.
    fn emit<F>(&mut self, sink: &mut F, left: &mut usize) -> XzResult<bool>
    where
        F: FnMut(u64, &[u8]),
    {
        let mut did = false;
        if let Some((buf, len, sent)) = self.part.as_mut() {
            let take = (*len - *sent).min(*left);
            if take != 0 {
                sink(self.emitted, &buf[*sent..*sent + take]);
                self.emitted += take as u64;
                *sent += take;
                *left -= take;
                did = true;
            }
            if *sent == *len {
                let (buf, _, _) = self.part.take().expect("just borrowed");
                buf.recycle_into(&mut self.spare_out);
            } else {
                return Ok(did);
            }
        }
        while *left != 0
            && let Some(done) = self.ready.remove(&self.next_emit)
        {
            let XzDone {
                index: _,
                stream,
                block_in_stream,
                file_offset,
                unpacked_len,
                res,
                out,
                lent: _,
                bytes,
                checks,
                window_err,
            } = done;
            self.spare_in.push(bytes);
            if let Err(kind) = res {
                out.recycle_into(&mut self.spare_out);
                let error = XzError::in_block(kind, stream, block_in_stream, file_offset);
                return Err(self.window_failed(error, window_err));
            }
            let take = unpacked_len.min(*left);
            sink(self.emitted, &out[..take]);
            self.emitted += take as u64;
            *left -= take;
            if let Some(c) = checks
                && !self.opts.plan.is_none()
            {
                self.checks.push(c);
            }
            if take == unpacked_len {
                out.recycle_into(&mut self.spare_out);
            } else {
                // The rest of this block goes out first on the next call,
                // ahead of anything decoded in the meantime.
                self.part = Some((out, unpacked_len, take));
            }
            self.next_emit += 1;
            did = true;
            if let Some(cap) = self.opts.max_unpack_bytes
                && self.emitted > cap
            {
                return Err(self.err(XzErrorKind::TooMuchOutput { limit: cap }));
            }
        }
        Ok(did)
    }
}

/// What a held block's output costs in RAM: nothing when it is on caller
/// memory.
fn in_ram(lent: bool, unpacked_len: usize) -> u64 {
    if lent { 0 } else { unpacked_len as u64 }
}

/// Which field of the index comes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexPhase {
    Count,
    Records,
    Padding,
    Crc,
}

/// How far the index has been read: everything before `pos` (counted from
/// the byte after the indicator) has been checked and hashed once.
struct IndexParse {
    pos: usize,
    /// The index's CRC-32 so far, the indicator included.
    crc: Crc32,
    /// The index's size so far, the indicator included.
    size: u64,
    phase: IndexPhase,
    records_left: u64,
    /// A record's first field, read before its second had arrived.
    unpadded: Option<u64>,
    /// The records read, folded as the blocks were.
    seen: IndexFold,
    /// Bytes hashed and records read, so a test can count the work.
    #[cfg(test)]
    hashed: u64,
    #[cfg(test)]
    records_parsed: u64,
}

impl IndexParse {
    fn new() -> Self {
        let mut crc = Crc32::new();
        crc.update(&[0x00]);
        IndexParse {
            pos: 0,
            crc,
            size: 1,
            phase: IndexPhase::Count,
            records_left: 0,
            unpadded: None,
            seen: IndexFold::default(),
            #[cfg(test)]
            hashed: 1,
            #[cfg(test)]
            records_parsed: 0,
        }
    }

    fn hash(&mut self, bytes: &[u8]) {
        self.crc.update(bytes);
        #[cfg(test)]
        {
            self.hashed += bytes.len() as u64;
        }
    }
}

/// What one turn of the state machine did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Something happened; go round again.
    Did,
    /// Nothing can happen without more input or a worker finishing.
    Blocked,
}

/// What came of offering a block to a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dispatch {
    Sent,
    /// There is room for it but it has not all arrived, or every worker is
    /// busy.
    Busy,
    /// It cannot go to a worker at all; the chase takes it.
    No,
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    #[cfg(feature = "enc")]
    #[test]
    fn recycled_packed_input_does_not_double_for_a_larger_block() {
        let props = crate::LzmaEncProps::new()
            .with_level(1)
            .with_dict_size(4096);
        let archive = crate::encode_xz(&[7; 4096], &props, CheckType::Crc64, 4096).unwrap();
        let header_size = super::super::block::header_size_from_first_byte(archive[12]).unwrap();
        let header = BlockHeader::parse(&archive[12..12 + header_size]).unwrap();
        let packed = header.compressed_size.unwrap();
        let padded = header_size + packed as usize + (4 - packed as usize % 4) % 4 + 8;
        let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(2));
        dec.flags.check = CheckType::Crc64;
        dec.check_size = 8;
        dec.buf = archive[12..12 + padded].to_vec();
        dec.spare_in.push(Vec::with_capacity(padded - 1));
        assert!(matches!(
            dec.try_dispatch(header_size, packed, 4096).unwrap(),
            Dispatch::Sent
        ));
        let done = dec.pool.as_ref().unwrap().collect().unwrap();
        done.res.unwrap();
        assert_eq!(done.bytes.capacity(), padded);
        assert_eq!(&done.out[..4096], &[7; 4096]);
    }

    /// Feeds `file` `step` bytes at a time and drains after every feed,
    /// collecting the output in order.
    fn decode_all(dec: &mut XzAdaptiveDecoder, file: &[u8], step: usize) -> XzResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut fed = 0;
        loop {
            if fed < file.len() {
                let end = file.len().min(fed + step);
                fed += dec.feed(&file[fed..end])?;
                if fed == file.len() {
                    dec.end_of_input();
                }
            }
            let status = dec.drain(|offset, bytes| {
                assert_eq!(offset, out.len() as u64);
                out.extend_from_slice(bytes);
            })?;
            match status {
                DrainStatus::Finished => return Ok(out),
                DrainStatus::NeedsMoreInput if fed == file.len() => {
                    dec.wait_for_worker();
                }
                _ => {}
            }
        }
    }

    #[cfg(feature = "enc")]
    fn sample(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| ((i / 7) as u8).wrapping_mul(31) ^ (i % 13) as u8)
            .collect()
    }

    #[cfg(feature = "enc")]
    fn archive(data: &[u8], block: u64) -> Vec<u8> {
        let props = crate::LzmaEncProps::new()
            .with_level(1)
            .with_dict_size(4096);
        crate::encode_xz(data, &props, CheckType::Crc64, block).unwrap()
    }

    #[test]
    fn a_first_block_over_the_limit_is_never_a_workers() {
        // A tiny block that declares a gigabyte of output: before, the first
        // block was sent whatever it cost, and the worker reserved and
        // zeroed all of it.
        let mut dec = XzAdaptiveDecoder::new(
            XzOptions::default()
                .with_threads(4)
                .with_memory_limit(1 << 20),
        );
        dec.buf = alloc::vec![0; 12];
        assert!(matches!(
            dec.try_dispatch(8, 4, 1 << 30).unwrap(),
            Dispatch::No
        ));
        assert_eq!(dec.cursor, 0);
        assert_eq!(dec.spawned_threads(), 0);
        // Nor once other blocks are held: it is chased when they are out.
        dec.ready.insert(1, completed(1));
        assert!(matches!(
            dec.try_dispatch(8, 4, 1 << 30).unwrap(),
            Dispatch::No
        ));
        // A block that fits alone still goes to a worker, or waits for one.
        assert!(matches!(
            dec.try_dispatch(8, 4, 64).unwrap(),
            Dispatch::Sent | Dispatch::Busy
        ));
    }

    #[cfg(feature = "enc")]
    #[test]
    fn a_block_over_the_limit_is_chased_and_decodes_the_same() {
        let data = sample(1 << 20);
        let file = archive(&data, 1 << 20);
        let mut dec = XzAdaptiveDecoder::new(
            XzOptions::default()
                .with_threads(4)
                .with_memory_limit(256 << 10),
        );
        let got = decode_all(&mut dec, &file, 64 << 10).unwrap();
        assert_eq!(got, data);
        assert_eq!(dec.spawned_threads(), 0, "the block was chased");
    }

    #[cfg(feature = "enc")]
    #[test]
    fn blocks_that_fit_still_go_to_workers_and_decode_the_same() {
        let data = sample(256 << 10);
        let file = archive(&data, 32 << 10);
        let mut dec = XzAdaptiveDecoder::new(
            XzOptions::default()
                .with_threads(4)
                .with_memory_limit(1 << 20),
        );
        let got = decode_all(&mut dec, &file, file.len()).unwrap();
        assert_eq!(got, data);
        assert!(dec.spawned_threads() > 0);
    }

    #[cfg(feature = "enc")]
    #[test]
    fn an_index_fed_a_byte_at_a_time_is_read_once() {
        const BLOCKS: usize = 3000;
        let data = sample(BLOCKS * 16);
        let file = archive(&data, 16);
        let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(1));
        let got = decode_all(&mut dec, &file, 1).unwrap();
        assert_eq!(got, data);
        // Every index byte but its stored CRC-32 is hashed exactly once, and
        // every record is read once: the work is linear in the index, not
        // quadratic in how many pieces it arrived in.
        assert!(dec.index_size > 4 * BLOCKS as u64 / 2);
        assert_eq!(dec.index_parse.hashed, dec.index_size - 4);
        assert_eq!(dec.index_parse.records_parsed, BLOCKS as u64);
        // Fed whole, the same.
        let mut whole = XzAdaptiveDecoder::new(XzOptions::default().with_threads(1));
        assert_eq!(decode_all(&mut whole, &file, file.len()).unwrap(), data);
        assert_eq!(whole.index_size, dec.index_size);
    }

    #[cfg(feature = "enc")]
    #[test]
    fn a_bad_index_fails_the_same_however_it_is_split() {
        let data = sample(64 * 16);
        let file = archive(&data, 16);
        let index_end = file.len() - STREAM_FOOTER_SIZE;
        // A wrong stored CRC, a wrong record and a wrong count.
        for at in [index_end - 1, index_end - 10, index_end - 8 - 2 * 64] {
            let mut bad = file.clone();
            bad[at] ^= 0x01;
            let mut errors = Vec::new();
            for step in [1, 3, 7, 64, bad.len()] {
                let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(1));
                errors.push(decode_all(&mut dec, &bad, step).unwrap_err());
            }
            assert!(errors.iter().all(|e| *e == errors[0]), "{at}: {errors:?}");
        }
    }

    #[cfg(feature = "enc")]
    #[test]
    fn a_check_failure_is_returned_again_and_never_panics() {
        let data = sample(64 << 10);
        let file = archive(&data, 16 << 10);
        let blocks =
            crate::xz::index::block_table(&mut std::io::Cursor::new(&file), u64::MAX).unwrap();
        let first = blocks[0];
        let check_end = (first.file_offset + first.record.unpadded_size) as usize;
        let mut bad = file.clone();
        bad[check_end - 1] ^= 0xff;
        for threads in [1, 4] {
            let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(threads));
            let error = decode_all(&mut dec, &bad, bad.len()).unwrap_err();
            assert_eq!(error.kind, XzErrorKind::CheckMismatch, "{threads}");
            for _ in 0..2 {
                assert_eq!(
                    dec.drain(|_, _| panic!("no output after an error")),
                    Err(error)
                );
                assert_eq!(dec.drain_upto(1, |_, _| panic!("no output")), Err(error));
                assert_eq!(dec.feed(&[0]), Err(error));
                assert!(!dec.wait_for_worker());
            }
        }
        // The same file, intact, decodes.
        let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(4));
        assert_eq!(decode_all(&mut dec, &file, file.len()).unwrap(), data);
    }

    fn completed(index: u64) -> XzDone {
        XzDone {
            index,
            stream: 0,
            block_in_stream: index,
            file_offset: 0,
            unpacked_len: 64,
            res: Ok(()),
            out: DicBuf::Owned(alloc::vec![0; 64]),
            lent: false,
            bytes: alloc::vec![0; 12],
            checks: None,
            window_err: None,
        }
    }

    #[test]
    fn completed_blocks_keep_adaptive_admission_and_feed_budget() {
        let mut dec =
            XzAdaptiveDecoder::new(XzOptions::default().with_threads(3).with_memory_limit(240));
        dec.buf = alloc::vec![0; 12];
        dec.outstanding = 1;
        dec.outstanding_bytes = 76;
        dec.ready.insert(1, completed(1));
        dec.ready.insert(2, completed(2));
        assert_eq!(dec.in_flight_bytes(), 240);
        assert_eq!(dec.feed(&[0; 12]).unwrap(), 0);
        assert!(matches!(
            dec.try_dispatch(8, 4, 64).unwrap(),
            Dispatch::Busy
        ));
        assert_eq!(dec.cursor, 0);
        assert_eq!(dec.spawned_threads(), 0);

        // A higher thread ceiling must not evade the byte limit either.
        dec.set_threads(8);
        assert!(matches!(
            dec.try_dispatch(8, 4, 64).unwrap(),
            Dispatch::Busy
        ));
    }

    #[test]
    fn partially_delivered_output_keeps_its_full_admission_cost() {
        let mut dec =
            XzAdaptiveDecoder::new(XzOptions::default().with_threads(1).with_memory_limit(76));
        dec.buf = alloc::vec![0; 12];
        dec.ready.insert(0, completed(0));
        let mut left = 1;
        assert!(dec.emit(&mut |_, _| {}, &mut left).unwrap());
        assert_eq!(dec.in_flight_bytes(), 76);
        assert_eq!(dec.feed(&[0; 12]).unwrap(), 0);
        assert!(matches!(
            dec.try_dispatch(8, 4, 64).unwrap(),
            Dispatch::Busy
        ));
        let mut left = 63;
        assert!(dec.emit(&mut |_, _| {}, &mut left).unwrap());
        assert!(dec.part.is_none());
        assert_eq!(dec.in_flight_bytes(), 12);
        assert_eq!(dec.feed(&[0; 12]).unwrap(), 12);
    }
}
