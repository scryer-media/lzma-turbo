//! Decoding a stream that is still arriving.
//!
//! [`super`] decodes a stream the way 7-Zip does: hand it a reader, get bytes
//! out, and every thread the plan asked for runs for the whole call. That is
//! the fastest way to decode an archive that is already on disk, and it is
//! what the throughput numbers in `docs/perf-log.md` are measured against.
//!
//! It is the wrong shape for a caller decoding an archive while it downloads.
//! Such a caller wants to stay single-threaded while it is chasing the tail of
//! the byte flow — decode what has arrived, cheaply, with one decoder and no
//! blocks in flight — and to spread across threads only once a backlog of
//! fully-arrived runs has built up, and then to go back. It cannot block on a
//! reader, because there is nothing to read yet; it decides what to do next
//! from what it can see, so it needs to see the backlog.
//!
//! [`Lzma2AdaptiveDecoder`] is that shape: bytes go in with
//! [`feed`](Lzma2AdaptiveDecoder::feed), which never blocks, output comes out
//! of [`drain`](Lzma2AdaptiveDecoder::drain) as (offset, bytes) blocks, and
//! [`set_threads`](Lzma2AdaptiveDecoder::set_threads) changes the mode at the
//! next run boundary without re-decoding or re-buffering anything. Both modes
//! decode the identical runs found by the identical [`Lzma2RunScanner`], so
//! switching between them cannot change the output.
//!
//! C: nothing. See [`super::pool`] for why the `MtDec` ring could not be bent
//! into this shape, and `docs/porting.md` for the record of the deviation.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::ops::Range;
use std::sync::Arc;

use crate::error::{Error, FinishMode, Status};
use crate::lzma2::Lzma2Decoder;

use crate::lzma2::scan::{Lzma2Run, Lzma2RunScanner};
use crate::mt::Lzma2MtOptions;
#[cfg(feature = "crc")]
use crate::mt::checksum::{BlockChecks, ChecksumPlan, Segmenter};
use crate::mt::pool::{Done, Job, Pool, locate};
use crate::mt::segq::SegQueue;

/// How much of the single-threaded decoder's dictionary is filled before it is
/// handed to the caller. C: `props.outStep_ST`.
const OUT_STEP_ST: usize = 1 << 20;

/// The least the input buffer may claim of the memory limit, whatever share
/// of it the rest of the decode has taken. Small enough to be no burden under
/// the smallest limits anyone sets, large enough that a decoder is never fed a
/// stream a page at a time.
const MIN_BUF_BUDGET: u64 = 1 << 20;

/// The size a copied input piece is cut to when the budget allows more.
///
/// Small enough that the pieces of a stream are all the same size and so
/// recycle through the queue's pool, large enough that a run of any ordinary
/// size still spans only a handful of them.
const PIECE_TARGET: usize = 4 << 20;

/// Why [`Lzma2AdaptiveDecoder::drain`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainStatus {
    /// Everything that could be decoded from the bytes fed so far has been.
    /// Feed more.
    NeedsMoreInput,
    /// Output was produced and there may be more immediately available; the
    /// caller got control back so that it can reconsider the thread count.
    Progress,
    /// The stream's end marker was reached and every block has been delivered.
    Finished,
}

/// A block of decoded output, in the order the caller asked for.
type Ready = (u64, Vec<u8>, usize);

/// A block a limited drain handed out only part of: its offset, its buffer,
/// how much of the buffer is output, and how much of that has been delivered.
type Part = (u64, Vec<u8>, usize, usize);

/// What came of trying to hand the run at the cursor to a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dispatch {
    /// A worker has it.
    Sent,
    /// There is a run to dispatch but no room to dispatch it: every worker is
    /// busy, or the memory limit is reached with blocks still in flight. Wait
    /// for one rather than decoding it on the caller's thread.
    Busy,
    /// Nothing to dispatch *yet*: there is no complete run at the cursor. More
    /// input may change that, so a caller that would rather wait than
    /// serialise can wait here.
    None,
    /// Nothing to dispatch, and no amount of waiting will change it: one
    /// thread, a run the chase has already started, a run too large for the
    /// memory limit, or no thread could be spawned. The chase decoder has to
    /// take this one or nothing will ever move.
    Chase,
}

/// Diagnostic counters. Temporary: printed at drop when `LZMA_ADAPTIVE_STATS`
/// is set, so that a regression can be attributed to a mechanism rather than
/// guessed at.
#[derive(Default, Debug)]
struct Stats {
    sent: u64,
    busy: u64,
    none: u64,
    chase_threads1: u64,
    chase_st_in_run: u64,
    chase_too_big: u64,
    chase_no_room: u64,
    chase_pool_empty: u64,
    chase_steps_none_arm: u64,
    chase_steps: u64,
    worker_bytes: u64,
    parked_out: u64,
    dropped_out: u64,
    peak_held: u64,
    peak_buf_cap: u64,
    peak_out_flight: u64,
    peak_ready: u64,
    peak_spare: u64,
    refused_held: u64,
    max_outstanding: u64,
    in_reused: u64,
    in_fresh: u64,
    in_grown: u64,
}

/// An LZMA2 decoder that is fed input and switches between single- and
/// multi-threaded decoding while it runs.
pub struct Lzma2AdaptiveDecoder {
    stats: Stats,
    dict_prop: u8,
    memory_limit: u64,
    threads: usize,
    ordered: bool,
    chase: bool,

    // Input, as the pieces it was handed over in. Nothing before `cursor_in`
    // is retained, and a piece a worker still holds is freed when the worker
    // lets go rather than when the cursor passes it.
    segs: SegQueue,
    input_done: bool,

    // Run discovery.
    scanner: Lzma2RunScanner,
    pending: VecDeque<Lzma2Run>,
    /// Offset of the stream's end marker, once it has been found.
    stream_end: Option<u64>,

    // The one cursor both modes claim work at. Everything before it has been
    // claimed by exactly one of them, in stream order.
    cursor_in: u64,
    cursor_out: u64,
    next_index: u64,
    runs_claimed: u64,

    // The single-threaded chase decoder, built on first use.
    st: Option<Lzma2Decoder>,
    st_wr: usize,
    /// True while the single-threaded decoder is part way through a run. The
    /// threaded path must not claim a run it has started.
    st_in_run: bool,

    // The worker pool, spawned on first dispatch.
    pool: Option<Pool>,
    outstanding: usize,
    /// What the runs out with workers are charged for: each one's output
    /// buffer, as it was when it left. The job carries the figure and hands it
    /// back, so what comes off is exactly what went on.
    outstanding_bytes: u64,

    // Decoded blocks waiting for their turn.
    ready: BTreeMap<u64, Ready>,
    /// The block `drain_upto` stopped in the middle of. Delivered before
    /// anything else, so a limited drain cuts a block without reordering it.
    part: Option<Part>,
    /// The capacity of every decoded block held here, in `ready` and in
    /// `part`. Capacity, not length: a block half handed over still owns the
    /// whole buffer until the rest of it goes.
    ready_cap: u64,
    emitted_out: u64,
    spare_out: Vec<Vec<u8>>,
    /// The capacity parked in the two pools above.
    spare_cap: u64,
    /// The unpacked length of the runs out with workers. Charged and refunded
    /// from the figure the job carries, so it cannot drift from the buffers it
    /// stands for.
    outstanding_unpacked: u64,
    /// Output decoded but not yet handed over, by length: what a caller
    /// steering its read-ahead by [`Lzma2AdaptiveDecoder::in_flight_bytes`]
    /// is told about.
    ready_bytes: u64,
    /// What the chase decoder has produced on the calling thread.
    chase_bytes: u64,
    /// What the last run dispatched needed, unpacked and packed. What a parked
    /// buffer is worth keeping for, once the backlog is empty.
    last_unpacked: u64,
    /// Input allowed while no run size is known. Doubles on demand; see
    /// [`Lzma2AdaptiveDecoder::room_to_take`].
    scan_reserve: u64,
    last_packed: u64,

    // What each run's decoder checksummed, worker or chase alike. See
    // [`crate::checksum`]: this is computed where the bytes were produced,
    // never on the caller's thread as it drains.
    #[cfg(feature = "crc")]
    plan: ChecksumPlan,
    #[cfg(feature = "crc")]
    checks: Vec<BlockChecks>,
    /// The chase decoder's open segment run, carried across calls so a run it
    /// decodes in many steps yields the segments the split points ask for and
    /// not one per step.
    #[cfg(feature = "crc")]
    st_seg: Option<Segmenter>,

    /// A worker's error, held until everything before it has been delivered.
    failed: Option<(u64, Error)>,
    complete: bool,
    cancelled: bool,
}

impl Drop for Lzma2AdaptiveDecoder {
    fn drop(&mut self) {
        if std::env::var_os("LZMA_ADAPTIVE_STATS").is_some() {
            std::eprintln!(
                "ADAPTIVE_STATS chase_bytes={} {:?}",
                self.chase_bytes,
                self.stats
            );
        }
    }
}

impl core::fmt::Debug for Lzma2AdaptiveDecoder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Lzma2AdaptiveDecoder")
            .field("dict_prop", &self.dict_prop)
            .field("threads", &self.threads)
            .field("ordered", &self.ordered)
            .field("in_flight_bytes", &self.in_flight_bytes())
            .field("pending_runs", &self.pending.len())
            .field("outstanding", &self.outstanding)
            .finish_non_exhaustive()
    }
}

impl Lzma2AdaptiveDecoder {
    /// A decoder for a raw LZMA2 stream with dictionary property byte
    /// `dict_prop`.
    ///
    /// No threads are created here, and none are created until the first run
    /// is actually dispatched to one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedProps`] if `dict_prop > 40`.
    pub fn new(dict_prop: u8, options: &Lzma2MtOptions) -> Result<Self, Error> {
        if dict_prop > 40 {
            return Err(Error::UnsupportedProps);
        }
        Ok(Lzma2AdaptiveDecoder {
            stats: Stats::default(),
            dict_prop,
            memory_limit: options.memory_limit,
            threads: options.threads.max(1),
            ordered: true,
            chase: true,
            segs: SegQueue::default(),
            input_done: false,
            scanner: Lzma2RunScanner::new(),
            pending: VecDeque::new(),
            stream_end: None,
            cursor_in: 0,
            cursor_out: 0,
            next_index: 0,
            runs_claimed: 0,
            st: None,
            st_wr: 0,
            st_in_run: false,
            part: None,
            pool: None,
            outstanding: 0,
            outstanding_bytes: 0,
            ready: BTreeMap::new(),
            ready_cap: 0,
            emitted_out: 0,
            spare_out: Vec::new(),
            spare_cap: 0,
            outstanding_unpacked: 0,
            ready_bytes: 0,
            chase_bytes: 0,
            last_unpacked: 0,
            scan_reserve: MIN_BUF_BUDGET,
            last_packed: 0,
            #[cfg(feature = "crc")]
            plan: ChecksumPlan::none(),
            #[cfg(feature = "crc")]
            checks: Vec::new(),
            #[cfg(feature = "crc")]
            st_seg: None,
            failed: None,
            complete: false,
            cancelled: false,
        })
    }

    // -- mode ---------------------------------------------------------------

    /// Sets the ceiling on threads used from the next run boundary onwards.
    ///
    /// One means the next run is decoded inline on the calling thread: no
    /// worker is woken, nothing is dispatched, and the output is handed to the
    /// sink straight out of the decoder's dictionary. Already-dispatched runs
    /// are not recalled; already-spawned workers stay parked on their channel
    /// and cost nothing until the count goes back up.
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads.max(1);
    }

    /// The current thread ceiling.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// How many worker threads exist. Zero until the first dispatch.
    #[must_use]
    pub fn spawned_threads(&self) -> usize {
        self.pool.as_ref().map_or(0, Pool::spawned)
    }

    /// How many worker threads are running right now.
    ///
    /// Equal to [`Lzma2AdaptiveDecoder::spawned_threads`] until the decoder is
    /// cancelled or dropped, at which point it must fall to zero.
    #[must_use]
    pub fn live_threads(&self) -> usize {
        self.pool.as_ref().map_or(0, Pool::live)
    }

    /// Sets what each decoder - worker or chase - is to checksum over the
    /// bytes it produces, and where the consumer's boundaries fall.
    ///
    /// Takes effect for runs claimed after this call, so set it before the
    /// first [`Lzma2AdaptiveDecoder::drain`]. The checksums are computed
    /// wherever the bytes were produced and never on the thread draining the
    /// output; see [`crate::checksum`] for why that distinction is the
    /// whole point.
    #[cfg(feature = "crc")]
    pub fn set_checksum(&mut self, plan: &ChecksumPlan) {
        self.plan = plan.clone();
    }

    /// Everything checksummed since the last call.
    ///
    /// Entries appear as their runs finish, which with unordered delivery is
    /// not stream order; each carries its own absolute offset, so a
    /// [`crate::crc::CrcFolder`] can take them as they come. After the decode
    /// is complete this has returned one entry per run, tiling the output.
    #[cfg(feature = "crc")]
    pub fn take_checks(&mut self) -> Vec<BlockChecks> {
        if self.complete {
            self.close_st_seg();
        }
        core::mem::take(&mut self.checks)
    }

    /// Closes the chase decoder's open segment run, if it has one.
    #[cfg(feature = "crc")]
    fn close_st_seg(&mut self) {
        if let Some(seg) = self.st_seg.take() {
            let checks = seg.finish();
            if checks.len != 0 {
                self.checks.push(checks);
            }
        }
    }

    /// Whether the run at the cursor may be decoded on the calling thread
    /// while it is still arriving. On by default.
    ///
    /// The chase decoder is what lets output come out of a stream that is
    /// still being written: it decodes the run at the cursor chunk by chunk,
    /// without waiting for the whole of it. The price is that it holds the
    /// cursor while it does so, and no worker may claim a run until it is
    /// through, so a decoder that chases is a decoder that is not threading.
    ///
    /// For a stream that is already on disk that price buys nothing: the bytes
    /// are all there, and the only reason the run at the cursor is incomplete
    /// is that the caller has not fed the rest of it yet. Such a caller should
    /// turn chasing off, feed more, and let the workers have the whole run.
    ///
    /// Off does not mean never: a run too large for the memory limit, a run
    /// the chase has already started, a single-threaded decoder, and anything
    /// at all once [`end_of_input`](Lzma2AdaptiveDecoder::end_of_input) has
    /// been called are still decoded on the calling thread, because otherwise
    /// nothing would decode them.
    pub fn set_chase(&mut self, chase: bool) {
        self.chase = chase;
    }

    /// Whether the chase decoder may take the run at the cursor.
    #[must_use]
    pub fn chases(&self) -> bool {
        self.chase
    }

    /// Delivers blocks in stream order (the default), or as soon as they are
    /// decoded.
    ///
    /// Unordered delivery suits a sink that writes by offset: a run whose
    /// bytes arrived early can be written out while an earlier run is still
    /// being decoded. Every block carries its output offset either way.
    pub fn set_ordered(&mut self, ordered: bool) {
        self.ordered = ordered;
    }

    // -- what the caller decides on -----------------------------------------

    /// Complete runs that have arrived and not yet been claimed: the backlog.
    #[must_use]
    pub fn pending_runs(&self) -> usize {
        self.pending.len()
    }

    /// The complete runs that have arrived and not yet been claimed.
    #[must_use]
    pub fn backlog(&self) -> impl ExactSizeIterator<Item = &Lzma2Run> {
        self.pending.iter()
    }

    /// How many runs have been handed to a decoder, by either path.
    #[must_use]
    pub fn runs_claimed(&self) -> u64 {
        self.runs_claimed
    }

    /// Samples the accounting for the diagnostic counters.
    fn note_peak(&mut self) {
        let held = self.held_bytes();
        if held > self.stats.peak_held {
            self.stats.peak_held = held;
        }
        self.stats.peak_buf_cap = self.stats.peak_buf_cap.max(self.segs.held_bytes());
        self.stats.peak_out_flight = self.stats.peak_out_flight.max(self.outstanding_bytes);
        self.stats.peak_ready = self.stats.peak_ready.max(self.ready_cap);
        self.stats.peak_spare = self.stats.peak_spare.max(self.spare_cap);
        self.stats.max_outstanding = self.stats.max_outstanding.max(self.outstanding as u64);
    }

    /// Every byte of buffer the decoder is holding.
    ///
    /// The input buffer, the copy of each dispatched run's packed bytes, the
    /// buffer each run is being decoded into, the decoded blocks waiting for
    /// their turn, and the capacity parked for reuse. Capacity throughout, not
    /// length: a buffer grown for a large run goes on holding that much memory
    /// until it is dropped, and a caller that asked for a limit wants to hear
    /// about it.
    ///
    /// This is the figure the memory limit is kept under, and it is always at
    /// least [`Lzma2AdaptiveDecoder::in_flight_bytes`], which counts the
    /// bytes rather than the buffers under them.
    ///
    /// What it does not include is the fixed cost of a decoder: the chase
    /// decoder's dictionary and each worker's, which are a function of the
    /// stream's dictionary size and the thread count rather than of how much
    /// is in flight, and which the limit therefore does not govern.
    #[must_use]
    pub fn held_bytes(&self) -> u64 {
        self.segs.held_bytes() + self.outstanding_bytes + self.ready_cap + self.spare_cap
    }

    /// Bytes in flight: input buffered and not yet claimed, the runs workers
    /// are decoding, and output decoded but not yet handed over.
    ///
    /// This is a gauge of the work in the decoder, which is what a caller
    /// deciding how far to read ahead wants; it is not what the decoder is
    /// holding, which is [`Lzma2AdaptiveDecoder::held_bytes`] and is the
    /// figure the memory limit is kept under. Dispatch is refused rather than
    /// allowed to push *that* over [`Lzma2AdaptiveDecoder::memory_limit`]; a
    /// run too large to fit at all is decoded by the single-threaded path,
    /// which streams it, instead of stalling.
    #[must_use]
    pub fn in_flight_bytes(&self) -> u64 {
        self.segs.len() + self.outstanding_unpacked + self.ready_bytes
    }

    /// How much of the output was decoded by the chase decoder, on the thread
    /// that called [`drain`](Lzma2AdaptiveDecoder::drain), rather than by a
    /// worker.
    ///
    /// The chase decoder holds the cursor while it works, so a decoder that is
    /// chasing is a decoder that is not threading. A caller that expected its
    /// runs to go to workers can read this and find out that they did not, and
    /// how much of the stream that cost; nothing else it can see says so.
    #[must_use]
    pub fn chase_decoded_bytes(&self) -> u64 {
        self.chase_bytes
    }

    /// The limit [`Lzma2AdaptiveDecoder::in_flight_bytes`] is kept under.
    #[must_use]
    pub fn memory_limit(&self) -> u64 {
        self.memory_limit
    }

    // -- input --------------------------------------------------------------

    /// Adds `data` to the stream and returns how much of it was taken.
    ///
    /// Never blocks and never decodes. Takes less than all of `data` only when
    /// the memory limit is reached, in which case the caller should drain and
    /// feed the rest.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`] after [`Lzma2AdaptiveDecoder::cancel`].
    pub fn feed(&mut self, data: &[u8]) -> Result<usize, Error> {
        let take = self.room_to_take(data.len(), false)?;
        if take == 0 {
            return Ok(0);
        }
        // Into a buffer the decode has finished with where there is one: on a
        // stream of large runs this is the difference between faulting in
        // every page of the input once and faulting in none of them.
        let mut buf = match self.segs.take_spare() {
            Some(buf) => {
                self.stats.in_reused += 1;
                buf
            }
            None => {
                self.stats.in_fresh += 1;
                Vec::new()
            }
        };
        buf.clear();
        // In pieces of a size that repeats. A copy sized to whatever room
        // happens to be free makes every piece a different size, so a parked
        // one rarely fits the next and the allocator maps and unmaps a large
        // region per piece; a steady size means the same few allocations serve
        // the whole stream. A buffer already in hand is used to its capacity,
        // whatever that is - it is mapped either way.
        let take = take.min(PIECE_TARGET.max(buf.capacity()));
        self.stats.in_grown += u64::from(buf.capacity() < take);
        buf.try_reserve_exact(take).map_err(|_| Error::Alloc)?;
        buf.extend_from_slice(&data[..take]);
        self.segs.push_owned(buf);
        Ok(take)
    }

    /// Hands back an allocation the decode has finished with, emptied.
    ///
    /// A caller that hands buffers over with
    /// [`feed_owned`](Lzma2AdaptiveDecoder::feed_owned) has given the decoder
    /// the allocation, and reading the next piece into a fresh one costs a
    /// page fault per page. This returns one the decoder is done with, cleared
    /// and with its capacity intact, to be filled and handed over again.
    ///
    /// A piece the decoder or one of its workers is still reading is never
    /// returned here, and neither is a range a caller lent with
    /// [`feed_shared`](Lzma2AdaptiveDecoder::feed_shared), which was never the
    /// decoder's to give. `None` means there is nothing spare, and the caller
    /// should allocate.
    #[must_use]
    pub fn reclaim_piece(&mut self) -> Option<Vec<u8>> {
        self.segs.take_spare()
    }

    /// Adds a piece of the stream the decoder takes ownership of.
    ///
    /// Nothing is copied: the piece becomes the decoder's, workers are handed
    /// references to it rather than copies of the runs inside it, and it is
    /// freed when the cursor and every worker are past it.
    ///
    /// Returns `None` when the whole piece was taken, and the piece itself,
    /// unchanged, when there was no room for it - a caller that is refused
    /// should drain and offer the same piece again. It is all or nothing, so
    /// that the path that accepts never copies.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`] after [`Lzma2AdaptiveDecoder::cancel`].
    pub fn feed_owned(&mut self, seg: Vec<u8>) -> Result<Option<Vec<u8>>, Error> {
        if self.room_to_take(seg.len(), true)? < seg.len() {
            return Ok(Some(seg));
        }
        self.segs.push_owned(seg);
        Ok(None)
    }

    /// Adds a piece of the stream the caller is keeping a reference to.
    ///
    /// The decoder holds its own reference to the same allocation for as long
    /// as any of it is unclaimed, so the caller may keep using the buffer
    /// elsewhere but must not write over the range it lent. What it costs the
    /// memory limit is the bytes lent, not the allocation, because the caller
    /// was holding that anyway.
    ///
    /// Returns `None` when the range was taken and the range itself when there
    /// was no room for it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`] after [`Lzma2AdaptiveDecoder::cancel`],
    /// and [`Error::InternalFailure`] if the range is not inside the buffer.
    pub fn feed_shared(
        &mut self,
        seg: &Arc<Vec<u8>>,
        range: Range<usize>,
    ) -> Result<Option<Range<usize>>, Error> {
        if range.end > seg.len() || range.start > range.end {
            return Err(Error::InternalFailure);
        }
        let len = range.end - range.start;
        if self.room_to_take(len, true)? < len {
            return Ok(Some(range));
        }
        self.segs.push_shared(seg, range);
        Ok(None)
    }

    /// How much of an offered `len` bytes the decoder has room for.
    ///
    /// The one rule all three entry points share: let go of what the decode
    /// has finished with, then take what the input budget leaves, measured
    /// against what the queue is charged rather than what is still unclaimed -
    /// a piece the cursor is halfway through is whole memory until the whole
    /// of it has been claimed. A caller handing over a whole piece gets all of
    /// it or none of it, because half a buffer is not something it can hold
    /// back.
    ///
    /// A decoder that can do nothing with what it holds is allowed more than
    /// the budget. The budget keeps read-ahead from crowding out the decode by
    /// setting aside what every thread could want at once; when no thread
    /// wants anything, most of that is set aside against nothing, and
    /// refusing would not save memory - it would leave the caller with input
    /// to give and the decoder with nothing to do but chase it a step at a
    /// time on the calling thread. So the allowance is the same budget
    /// reckoned for one worker instead of all of them: enough to keep the
    /// reader going, and still a whole dispatch in reserve, which is what
    /// stops read-ahead from filling the limit and forcing the chase it was
    /// trying to avoid.
    ///
    /// Nothing here can refuse its way into a stall. The budget's floor is
    /// the run the decoder is waiting on, a header, and whatever is charged
    /// but already read, so there is always room for the bytes that complete
    /// that run - and a run larger than the limit is allowed for by the same
    /// floor, which is written in the run's size and not in the limit.
    fn room_to_take(&mut self, len: usize, whole: bool) -> Result<usize, Error> {
        if self.cancelled {
            return Err(Error::Cancelled);
        }
        if len == 0 || self.complete {
            return Ok(0);
        }
        self.release_input();
        // Parked allocations do not stand in the way of the next piece: a copy
        // goes straight into one, so counting it twice - once where it sits
        // and again in the room the piece needs - would refuse input for
        // memory that is about to be the input.
        let held = self.segs.held_bytes() - self.segs.spare_bytes();
        let mut ceiling = self.buf_budget();
        if !self.has_work_in_hand() {
            ceiling = ceiling.max(self.buf_budget_for(1));
        }
        let mut room = ceiling.saturating_sub(held);
        // Before the scanner has seen a whole run there is no run size to
        // reckon a budget from, and how much input the first one needs is a
        // property of the stream, not of the limit. Rather than hand out half
        // the limit against that unknown - which a read-ahead caller takes
        // immediately and which is then the peak for the whole decode - the
        // scan reserve starts small and doubles each time the caller has
        // filled it and the decoder still has nothing to do with what it
        // holds. The stream's own first run is what stops the doubling: once
        // its size is known the ordinary budget takes over, so the peak
        // settles just above what finding that run actually cost.
        //
        // A caller handing over a whole piece needs room for all of it or it
        // is refused outright, so that is what the reserve has to reach for.
        let need = if whole { len as u64 } else { 1 };
        while room < need
            && self.known_run() == 0
            && !self.has_work_in_hand()
            && self.scan_reserve < self.input_free() / 2
        {
            self.scan_reserve = self
                .scan_reserve
                .saturating_mul(2)
                .min(self.input_free() / 2);
            let mut ceiling = self.buf_budget();
            if !self.has_work_in_hand() {
                ceiling = ceiling.max(self.buf_budget_for(1));
            }
            room = ceiling.saturating_sub(held);
        }
        let mut take = usize::try_from(room).unwrap_or(usize::MAX).min(len);
        if whole && take < len && !self.has_work_in_hand() {
            // A piece handed over whole cannot be cut down to the budget, and
            // the decoder has nothing else it could be getting on with, so the
            // only question left is whether the limit has room for it at all.
            // Anything narrower than that deadlocks a caller whose pieces are
            // a little larger than the budget happens to be - the budget is
            // written in run sizes, and before a run is known there are none.
            // Only for the first pieces, though: the point is to get a caller
            // that cannot cut its pieces up past a budget that is smaller than
            // one of them, not to let read-ahead take the limit and leave the
            // decode with no room to run in.
            let fits = self.input_free().saturating_sub(held);
            if len as u64 <= fits && held < (len as u64).saturating_mul(2) {
                take = len;
            }
        }
        if whole && take < len {
            return Ok(0);
        }
        Ok(take)
    }

    /// Whether the decoder has something to do that does not need more input.
    ///
    /// Anything outstanding or decoded is work: a worker finishing frees its
    /// buffer and a drain frees a block, and either lets the next feed in. So
    /// is a whole run in hand, which a worker can take.
    ///
    /// The chase is not work in this sense, though it can always be run.
    /// Counting it would make the decoder refuse the input that would let it
    /// dispatch, on the grounds that it could decode the stream one step at a
    /// time on the calling thread instead - which is precisely the outcome
    /// worth avoiding.
    fn has_work_in_hand(&self) -> bool {
        if self.outstanding > 0 || !self.ready.is_empty() {
            return true;
        }
        self.pending
            .front()
            .is_some_and(|r| r.in_offset + r.packed_len <= self.segs.end())
    }

    /// What the limit leaves for input once everything else the decoder is
    /// holding is counted.
    fn input_free(&self) -> u64 {
        let other = self.held_bytes() - self.segs.held_bytes();
        self.memory_limit.saturating_sub(other)
    }

    /// The packed size of the run the decoder is about to claim, or zero
    /// before it has seen one. What every rule about the input buffer is
    /// written in terms of, and what none of them can be written without.
    fn known_run(&self) -> u64 {
        self.pending
            .front()
            .map_or(0, |r| r.packed_len)
            .max(self.last_packed)
    }

    fn buf_budget(&self) -> u64 {
        self.buf_budget_for(self.threads as u64)
    }

    /// The input's share of the limit when `slots` workers could be at work.
    fn buf_budget_for(&self, slots: u64) -> u64 {
        let free = self.input_free();
        let run_in = self.known_run();
        let run_out = self
            .pending
            .front()
            .map_or(0, |r| r.unpacked_len)
            .max(self.last_unpacked);
        let work = run_in.saturating_add(run_out).saturating_mul(slots);
        // Nothing is known about the runs yet: the scan reserve says how much
        // input to accept against that, and it is what grows when the scan
        // needs more. See `room_to_take`.
        let unknown = run_in == 0 && run_out == 0;
        // A run's packed bytes are not quite enough to claim it: the scanner
        // has to see where the next run starts before it will say the last one
        // is complete, so the floor carries a header's worth beyond it.
        //
        // And it carries what is charged but cannot be decoded - the front of
        // the piece the cursor is inside, and the pieces a worker has not let
        // go of. Without that the decoder stalls one run short every time: it
        // claims a run out of the middle of a piece, is charged for the whole
        // piece, and has no budget left for the bytes that would complete the
        // next one. This does not grow without bound, because a floor that
        // only ever admits the bytes the next run is short of keeps the pieces
        // it takes that small.
        let floor = self
            .segs
            .dead_bytes()
            .saturating_add(run_in)
            .saturating_add(MIN_BUF_BUDGET);
        let share = if unknown {
            self.scan_reserve
        } else {
            free.saturating_sub(work)
        };
        share.min(free / 2).max(floor).min(free)
    }

    /// Declares that no more input is coming. A stream that then does not end
    /// with its end marker is reported as corrupt rather than waited on.
    pub fn end_of_input(&mut self) {
        self.input_done = true;
    }

    /// Stops the decode: workers finish whatever run they hold, are told to do
    /// no more, and are joined before this returns.
    pub fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(p) = self.pool.as_mut() {
            p.shutdown();
        }
        self.pool = None;
        self.outstanding = 0;
        self.outstanding_bytes = 0;
        self.outstanding_unpacked = 0;
        // The pool is shut down above, so nothing else holds the input any
        // more and all of it can go at once.
        self.segs.clear();
    }

    // -- output -------------------------------------------------------------

    /// Blocks until a worker hands back a finished run, and takes it in.
    ///
    /// Returns false at once when no run is outstanding.
    ///
    /// [`drain`](Lzma2AdaptiveDecoder::drain) hands control back as soon as
    /// there is nothing it can do without more input, even with workers still
    /// decoding, so that the caller can feed the next run rather than wait for
    /// the last one. A caller whose read-ahead is already satisfied has nothing
    /// to feed and nothing else to do; without somewhere to wait it would call
    /// drain again, and again, for as long as the workers took. This is that
    /// place: one block, no poll interval, no deadline.
    ///
    /// Everything else a worker has already finished is taken in along with
    /// the run waited for. A run that failed is not reported here - the failure
    /// is held with the block it belongs to and comes out of the next drain, in
    /// order, exactly as it would have without this call.
    ///
    /// False also comes back when the outstanding run can no longer arrive: the
    /// decode was cancelled, or every worker has gone. A caller looping on this
    /// therefore cannot spin - the next drain turns that state into the error
    /// it is.
    pub fn wait_for_worker(&mut self) -> bool {
        if self.outstanding == 0 {
            return false;
        }
        self.collect(true)
    }

    /// Decodes as much as the bytes fed so far allow and hands each block to
    /// `sink` as `(output offset, bytes)`.
    ///
    /// Returns when there is nothing left to do without more input, when the
    /// stream is finished, or when a block has been delivered and the caller
    /// might want to reconsider the thread count. It blocks only while a
    /// worker is decoding and there is nothing else to get on with.
    ///
    /// # Errors
    ///
    /// Returns the located [`Error::CorruptRun`] for a run that does not
    /// decode, but only after every block before it has been delivered, so a
    /// caller writing by offset keeps what it has already written.
    pub fn drain<F>(&mut self, sink: F) -> Result<DrainStatus, Error>
    where
        F: FnMut(u64, &[u8]),
    {
        self.drain_impl(usize::MAX, sink)
    }

    /// As [`drain`](Lzma2AdaptiveDecoder::drain), but stops once `sink` has
    /// been handed `limit` bytes, keeping the rest of the block it was in the
    /// middle of for the next call.
    ///
    /// This is the shape a `Read` implementation wants: it is asked for as
    /// much as fits in a caller's buffer, which is rarely as much as a run of
    /// a parallel-encoded archive decodes to, and without a limit the
    /// difference has to be spilled into a growing buffer of its own. With
    /// one, the decoder's memory is a function of what is in flight rather
    /// than of what has been fed.
    ///
    /// A `limit` of zero delivers nothing. Ordering is unaffected: the part
    /// left over is the first thing the next call hands out.
    ///
    /// # Errors
    ///
    /// As [`drain`](Lzma2AdaptiveDecoder::drain).
    pub fn drain_upto<F>(&mut self, limit: usize, sink: F) -> Result<DrainStatus, Error>
    where
        F: FnMut(u64, &[u8]),
    {
        self.drain_impl(limit, sink)
    }

    fn drain_impl<F>(&mut self, limit: usize, mut sink: F) -> Result<DrainStatus, Error>
    where
        F: FnMut(u64, &[u8]),
    {
        if self.cancelled {
            return Err(Error::Cancelled);
        }
        let mut left = limit;
        let mut progress = false;
        loop {
            self.note_peak();
            self.scan()?;
            let mut did = self.collect(false);
            did |= self.emit(&mut sink, &mut left);
            self.check_failed()?;

            // The chase decoder must not steal a run that a worker could take,
            // or a decoder with four idle workers would do all the work on the
            // caller's thread. It takes over only when there is no complete
            // run at the cursor to dispatch, which is exactly the case it
            // exists for: the tail of the stream, still arriving.
            match self.dispatch()? {
                Dispatch::Sent => did = true,
                Dispatch::Busy => {}
                Dispatch::Chase => {
                    self.stats.chase_steps += 1;
                    if self.st_step(&mut sink, &mut left)? {
                        did = true;
                    }
                }
                Dispatch::None => {
                    // The chase serialises the whole decoder: while it holds
                    // the cursor no worker may claim a run. That is the right
                    // trade only when there is nothing else in flight - the
                    // tail of an arriving stream.
                    //
                    // With a worker outstanding there is something to wait
                    // for, and waiting costs a fraction of a block while
                    // chasing costs the whole of one, so the chase stands
                    // aside. A caller that has turned chasing off stands aside
                    // for the *input* too: the run at the cursor is incomplete
                    // only because not all of it has been fed, and feeding the
                    // rest is cheaper than decoding it here. Once the input is
                    // over, waiting is waiting forever, so the chase takes it
                    // either way.
                    let busy = self.outstanding != 0;
                    // Waiting for input is only waiting if input can still be
                    // taken: at the memory limit `feed` refuses everything, so
                    // a decoder that waited there would wait forever.
                    let waiting_for_input =
                        !self.chase && !self.input_done && self.held_bytes() < self.memory_limit;
                    if !busy && !waiting_for_input {
                        self.stats.chase_steps_none_arm += 1;
                        if self.st_step(&mut sink, &mut left)? {
                            did = true;
                        }
                    }
                }
            }

            progress |= did;

            if self.complete
                && self.outstanding == 0
                && self.ready.is_empty()
                && self.part.is_none()
            {
                return Ok(DrainStatus::Finished);
            }
            if left == 0 {
                // The caller's buffer is full. Whatever else could be decoded
                // waits for the next call, which starts with the part of the
                // block that did not fit.
                return Ok(DrainStatus::Progress);
            }
            if did {
                continue;
            }
            if self.outstanding != 0 {
                // A worker is busy and there is nothing else *here* to do.
                // Whether to wait for it depends on whether the caller has
                // something better to do: while input is still coming, handing
                // control back lets the next run be fed and given to the next
                // worker, where waiting here would decode one run at a time no
                // matter how many threads there are. Once the input is over,
                // or the memory limit means no more can be taken, there is
                // nothing better, so wait.
                if !self.input_done && self.held_bytes() < self.memory_limit {
                    progress |= did;
                    return Ok(if progress {
                        DrainStatus::Progress
                    } else {
                        DrainStatus::NeedsMoreInput
                    });
                }
                if self.collect(true) {
                    continue;
                }
                return Err(Error::InternalFailure);
            }
            if self.input_done && !self.complete {
                return Err(Error::CorruptData);
            }
            return Ok(if progress {
                DrainStatus::Progress
            } else {
                DrainStatus::NeedsMoreInput
            });
        }
    }

    // -- the machinery ------------------------------------------------------

    /// Walks whatever headers have arrived since the last call.
    fn scan(&mut self) -> Result<(), Error> {
        if self.scanner.finished() {
            return Ok(());
        }
        // The scanner keeps its own position and its own state between
        // calls, so input that arrived in four pieces is walked as four
        // pieces: a header split across a boundary is resumed, not copied
        // together first.
        while let Some(piece) = self.segs.piece_at(self.scanner.in_position()) {
            self.scanner.feed(piece)?;
            while let Some(r) = self.scanner.next_run() {
                self.pending.push_back(r);
            }
            if self.scanner.finished() {
                break;
            }
        }
        if self.scanner.finished() {
            self.stream_end = Some(self.scanner.in_position());
        }
        Ok(())
    }

    /// Lets go of every piece of input the decode has finished with.
    ///
    /// Cheap, and cheap on purpose: a piece whose last byte is behind the
    /// cursor is dropped, and nothing else is touched. There is no front to
    /// move down and no capacity to shrink, because the pieces were never
    /// copied into anything.
    fn release_input(&mut self) {
        // What a piece the stream is past may be kept for, and in how many
        // buffers: the same allowance the output pool works to, so that
        // recycling never sits on a large part of the limit it would otherwise
        // be dispatching with.
        let piece = self.segs.last_piece().max(MIN_BUF_BUDGET);
        let room = (self.memory_limit / 8).min(piece.saturating_mul(self.threads as u64 + 2));
        self.segs.set_park_budget(room, self.threads + 2);
        self.segs.retain_from(self.cursor_in);
    }

    /// Takes finished blocks off the pool, optionally waiting for one.
    fn collect(&mut self, block: bool) -> bool {
        let Some(pool) = self.pool.as_ref() else {
            return false;
        };
        let mut batch = Vec::new();
        if block && let Some(d) = pool.collect() {
            batch.push(d);
        }
        while let Some(d) = pool.try_collect() {
            batch.push(d);
        }
        let any = !batch.is_empty();
        for d in batch {
            self.accept(d);
        }
        any
    }

    fn accept(&mut self, d: Done) {
        // Charged and refunded from the figures the job carries, so these
        // cannot drift or go below zero: the unpacked length is the run's own,
        // and the input is charged to the queue, never to the job.
        self.outstanding_unpacked -= d.unpacked_len as u64;
        self.outstanding -= 1;
        self.outstanding_bytes -= d.held;
        // Dropping the job's references here is what lets the queue see that
        // the pieces this run spanned have no holder left.
        drop(d.packed);
        self.segs.sweep();
        match d.res {
            Ok(()) => {
                #[cfg(feature = "crc")]
                if let Some(c) = d.checks {
                    self.checks.push(c);
                }
                self.ready_cap += d.out.capacity() as u64;
                self.ready_bytes += d.unpacked_len as u64;
                self.ready
                    .insert(d.out_offset, (d.out_offset, d.out, d.unpacked_len));
            }
            Err(e) => {
                self.recycle(d.out);
                let located = locate(d.index, d.out_offset, e);
                match &self.failed {
                    Some((off, _)) if *off <= d.out_offset => {}
                    _ => self.failed = Some((d.out_offset, located)),
                }
            }
        }
    }

    /// Hands out every block whose turn has come, up to `left` bytes.
    ///
    /// A block that does not fit in what is left is cut: the rest stays in
    /// [`Self::part`] and is the first thing the next call delivers, so a
    /// limited drain changes how the output is sliced and nothing else.
    fn emit<F: FnMut(u64, &[u8])>(&mut self, sink: &mut F, left: &mut usize) -> bool {
        let mut any = false;
        loop {
            if *left == 0 {
                break;
            }
            if let Some((off, buf, len, sent)) = self.part.as_mut() {
                let take = (*len - *sent).min(*left);
                sink(*off + *sent as u64, &buf[*sent..*sent + take]);
                *sent += take;
                *left -= take;
                self.ready_bytes -= take as u64;
                any = true;
                if *sent == *len {
                    let (_, buf, _, _) = self.part.take().expect("just borrowed");
                    self.ready_cap -= buf.capacity() as u64;
                    self.recycle(buf);
                }
                continue;
            }
            let key = if self.ordered {
                match self.ready.first_key_value() {
                    Some((k, _)) if *k == self.emitted_out => *k,
                    _ => break,
                }
            } else {
                match self.ready.first_key_value() {
                    Some((k, _)) => *k,
                    None => break,
                }
            };
            let (off, buf, len) = self.ready.remove(&key).expect("just looked it up");
            let take = len.min(*left);
            self.ready_bytes -= take as u64;
            sink(off, &buf[..take]);
            *left -= take;
            if off == self.emitted_out {
                // The whole block counts as claimed even when only part of it
                // has been handed over: the rest is held here, ahead of
                // everything else, so nothing can overtake it.
                self.emitted_out = off + len as u64;
            }
            if take == len {
                self.ready_cap -= buf.capacity() as u64;
                self.recycle(buf);
            } else {
                // Still counted: the block moves from `ready` to `part`, and
                // the buffer under it is held either way.
                self.part = Some((off, buf, len, take));
            }
            any = true;
        }
        any
    }

    fn check_failed(&mut self) -> Result<(), Error> {
        if let Some((off, e)) = self.failed {
            // In order, everything before the bad run has now been delivered;
            // out of order there is nothing to wait for.
            if !self.ordered || self.emitted_out >= off {
                self.cancel();
                self.cancelled = false;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Parks an output buffer for the next run, or lets it go.
    fn recycle(&mut self, buf: Vec<u8>) {
        // The run at the cursor is what the buffer would be reused for. When
        // there is none - a caller feeding one run at a time has nothing in
        // the backlog most of the time - the last run dispatched says what the
        // runs of this stream are like. A chase step writes at most one step's
        // worth, so a buffer that size is worth keeping either way.
        let want = self
            .pending
            .front()
            .map_or(self.last_unpacked, |r| r.unpacked_len)
            .max(OUT_STEP_ST as u64);
        let held = self.spare_out.len();
        if held < self.threads + 2 && self.worth_parking(buf.capacity(), want, held) {
            self.stats.parked_out += 1;
            self.spare_cap += buf.capacity() as u64;
            self.spare_out.push(buf);
        } else {
            self.stats.dropped_out += 1;
        }
    }

    /// Whether a buffer of `cap` bytes is worth parking when the next run
    /// wants `want` of them.
    ///
    /// Recycling a buffer saves an allocation and a page fault per run, which
    /// is worth having, but only while the buffer is about the size the next
    /// run asks for. One grown for a sixty-megabyte run is sixty megabytes of
    /// the caller's limit sitting idle once the runs are a megabyte, and since
    /// the limit counts what is parked, it is also sixty megabytes that the
    /// next dispatch cannot have. Two bounds fall out of that: a buffer far
    /// larger than the work in front of it is dropped, and what is parked
    /// altogether stays a small part of the limit - an eighth of it, or the
    /// buffers the threads in use could want at once, whichever is the less.
    ///
    /// Dropping is not free either: the next run allocates again and faults
    /// the pages back in one by one, which a decode of small runs does
    /// thousands of times. So the cap sheds the buffers that are too large for
    /// the work rather than the pool itself, and the last buffer of each kind
    /// is kept whatever the cap says - one of each is what recycling needs,
    /// and it is what the next run will ask for.
    fn worth_parking(&self, cap: usize, want: u64, held: usize) -> bool {
        let cap = cap as u64;
        if want != 0 && cap > want.saturating_mul(2) {
            return false;
        }
        if held == 0 {
            return true;
        }
        let cap_room = (self.memory_limit / 8).min(
            want.max(OUT_STEP_ST as u64)
                .saturating_mul(self.threads as u64),
        );
        self.spare_cap + cap <= cap_room
    }

    /// Takes a parked output buffer, if there is one.
    fn take_out(&mut self) -> Vec<u8> {
        match self.spare_out.pop() {
            Some(b) => {
                self.spare_cap -= b.capacity() as u64;
                b
            }
            None => Vec::new(),
        }
    }

    /// Sends the run at the cursor to a worker, if that is the right thing to
    /// do with it.
    fn dispatch(&mut self) -> Result<Dispatch, Error> {
        if self.complete || self.failed.is_some() {
            return Ok(Dispatch::None);
        }
        if self.threads <= 1 {
            self.stats.chase_threads1 += 1;
            return Ok(Dispatch::Chase);
        }
        if self.st_in_run {
            self.stats.chase_st_in_run += 1;
            return Ok(Dispatch::Chase);
        }
        let Some(run) = self.pending.front().copied() else {
            self.stats.none += 1;
            return Ok(Dispatch::None);
        };
        if run.in_offset != self.cursor_in {
            self.stats.none += 1;
            return Ok(Dispatch::None);
        }
        if self.outstanding >= self.threads {
            self.stats.busy += 1;
            return Ok(Dispatch::Busy);
        }
        let unpacked = usize::try_from(run.unpacked_len).map_err(|_| Error::Alloc)?;

        // A run that cannot fit inside the limit at all is left to the
        // single-threaded path, which streams it out of its dictionary a
        // megabyte at a time. Waiting for room that will never appear would be
        // a deadlock, and buffering it anyway would be a lie about the limit.
        //
        // A run needs a buffer to decode into and a copy of its packed bytes,
        // both of which the accounting counts for as long as the worker has
        // them. What it costs on top of what is already held, though, is only
        // what the buffers it will reuse do not already cover: those are parked
        // capacity, counted where they sit, and dispatch moves them rather than
        // allocating more.
        let size = run.unpacked_len + run.packed_len;
        if size > self.memory_limit {
            self.stats.chase_too_big += 1;
            return Ok(Dispatch::Chase);
        }
        if !self.room_for(run) {
            // Room appears when an outstanding block lands. If none is
            // outstanding there is nothing to wait for, so the chase decoder
            // takes it and streams it instead.
            self.stats.refused_held += self.held_bytes();
            return Ok(if self.outstanding == 0 {
                self.stats.chase_no_room += 1;
                Dispatch::Chase
            } else {
                self.stats.busy += 1;
                Dispatch::Busy
            });
        }

        let Some(claim) = self
            .segs
            .claim(run.in_offset..run.in_offset + run.packed_len)
        else {
            return Ok(Dispatch::None);
        };

        let pool = match self.pool.as_mut() {
            Some(p) => p,
            None => {
                self.pool = Some(Pool::new(self.dict_prop));
                self.pool.as_mut().expect("just set")
            }
        };
        if pool.spawned() < self.threads && pool.spawned() <= self.outstanding {
            pool.grow();
        }
        if pool.spawned() == 0 {
            // No thread could be created; fall back to decoding inline.
            self.stats.chase_pool_empty += 1;
            return Ok(Dispatch::Chase);
        }

        let out = self.take_out();
        // What the job costs while the worker has it. Its input costs nothing
        // here: the worker was handed references to input the queue is
        // already charged for, and the queue goes on charging for it until
        // the worker lets go. What is left is the buffer the run is decoded
        // into, which will be grown to the run's length if it is not there
        // already, so it is charged for whichever is the larger.
        let held = (out.capacity() as u64).max(run.unpacked_len);

        let pool = self.pool.as_mut().expect("checked above");
        pool.dispatch(Job {
            #[cfg(feature = "crc")]
            plan: self.plan.clone(),
            index: self.next_index,
            out_offset: run.out_offset,
            unpacked_len: unpacked,
            packed: claim,
            out,
            held,
        })?;

        self.stats.sent += 1;
        self.stats.worker_bytes += run.unpacked_len;
        self.outstanding_unpacked += run.unpacked_len;
        self.outstanding += 1;
        self.outstanding_bytes += held;
        self.last_unpacked = run.unpacked_len;
        self.last_packed = run.packed_len;
        self.next_index += 1;
        self.runs_claimed += 1;
        self.pending.pop_front();
        self.cursor_in += run.packed_len;
        self.cursor_out += run.unpacked_len;
        self.note_end();
        self.release_input();
        Ok(Dispatch::Sent)
    }

    /// Whether there is room to hand `run` to a worker.
    ///
    /// A run needs a buffer to decode into, which the accounting counts for
    /// as long as the worker has it. Its input costs nothing on top: the
    /// worker reads the pieces the queue is holding anyway. What the buffer
    /// costs on top of what is already held is only what the one it will reuse
    /// does not already cover, because that is parked capacity, counted where
    /// it sits, and dispatch moves it rather than allocating more.
    fn room_for(&self, run: Lzma2Run) -> bool {
        let reuse_out = self.spare_out.last().map_or(0, |b| b.capacity() as u64);
        let need = run.unpacked_len.saturating_sub(reuse_out);
        self.held_bytes() + need <= self.memory_limit
    }

    /// Decodes on the calling thread: one step of at most
    /// [`OUT_STEP_ST`] bytes, over whatever input has arrived.
    ///
    /// This is the chase path. It decodes a run whose tail has not arrived
    /// yet, chunk by chunk, and keeps its state between calls, so there is no
    /// need to wait for a run to be complete before starting on it.
    fn st_step<F: FnMut(u64, &[u8])>(
        &mut self,
        sink: &mut F,
        left: &mut usize,
    ) -> Result<bool, Error> {
        if self.complete || self.failed.is_some() || *left == 0 {
            return Ok(false);
        }
        if self.segs.piece_at(self.cursor_in).is_none() {
            return Ok(false);
        }
        if self.st.is_none() {
            self.st = Some(Lzma2Decoder::new(self.dict_prop)?);
            self.st_wr = 0;
        }
        let dec = self.st.as_mut().expect("just built");

        let dic_pos = dec.dic_pos();
        // A step is capped by what the caller still has room for as well as by
        // the step size, so a limited drain never decodes bytes it cannot hand
        // over and would have to hold.
        let step = OUT_STEP_ST.min(*left);
        let mut limit = dec.dic_buf_size();
        if limit - self.st_wr > step {
            limit = self.st_wr + step;
        }
        // One contiguous piece per step. The block decoder keeps its state
        // between calls, so a run spanning four pieces is decoded in four
        // steps and nothing is copied to make it look like one.
        let piece = self.segs.piece_at(self.cursor_in).expect("checked above");
        let (used, status) = dec.decode_block(limit, piece, FinishMode::Any)?;
        let produced = dec.dic_pos() - dic_pos;

        if produced != 0 {
            let start = self.st_wr;
            let end = dec.dic_pos();
            let offset = self.cursor_out;
            #[cfg(feature = "crc")]
            if !self.plan.is_none() {
                // A run the chase decoder takes over several steps must still
                // be cut only where the split points say, so the segmenter is
                // carried across steps and restarted only when the chase path
                // resumes somewhere else.
                if self.st_seg.as_ref().is_some_and(|s| s.pos() != offset) {
                    // Inlined `close_st_seg`: these are disjoint fields, and
                    // the decoder is borrowed across this block.
                    if let Some(seg) = self.st_seg.take() {
                        let checks = seg.finish();
                        if checks.len != 0 {
                            self.checks.push(checks);
                        }
                    }
                }
                let plan = self.plan.clone();
                self.st_seg
                    .get_or_insert_with(|| Segmenter::new(&plan, offset))
                    .update(dec.dic_slice(start, end));
            }
            if self.ordered && self.emitted_out != offset {
                // Blocks dispatched earlier have not landed yet, so this one
                // has to wait its turn in memory.
                let mut b = match self.spare_out.pop() {
                    Some(b) => {
                        self.spare_cap -= b.capacity() as u64;
                        b
                    }
                    None => Vec::new(),
                };
                b.clear();
                b.extend_from_slice(dec.dic_slice(start, end));
                let len = b.len();
                self.ready_cap += b.capacity() as u64;
                self.ready_bytes += len as u64;
                self.ready.insert(offset, (offset, b, len));
            } else {
                sink(offset, dec.dic_slice(start, end));
                self.emitted_out = offset + produced as u64;
                *left -= produced.min(*left);
            }
            dec.wrap_dic_pos();
            self.st_wr = dec.dic_pos();
        }

        self.chase_bytes += produced as u64;
        self.cursor_in += used as u64;
        self.cursor_out += produced as u64;
        self.retire_runs();
        self.note_end();

        if status == Status::FinishedWithMark {
            self.complete = true;
            self.stream_end = Some(self.cursor_in);
        }
        if used == 0 && produced == 0 {
            if self.input_done && !self.complete {
                return Err(Error::CorruptData);
            }
            return Ok(false);
        }
        self.release_input();
        Ok(true)
    }

    /// Drops runs the single-threaded path has decoded past, and works out
    /// whether the cursor is at a boundary.
    fn retire_runs(&mut self) {
        while let Some(front) = self.pending.front().copied() {
            if front.in_offset + front.packed_len <= self.cursor_in {
                self.pending.pop_front();
                self.runs_claimed += 1;
                self.next_index += 1;
            } else {
                break;
            }
        }
        self.st_in_run = match self.pending.front() {
            Some(front) => self.cursor_in > front.in_offset,
            // Past every complete run: either inside the run still arriving,
            // or exactly at the end marker.
            None => self.stream_end.is_none_or(|e| self.cursor_in + 1 < e),
        };
    }

    /// Notices that the threaded path has claimed the last run there is.
    fn note_end(&mut self) {
        if let Some(end) = self.stream_end
            && self.pending.is_empty()
            && !self.st_in_run
            && self.cursor_in + 1 >= end
            // The cursor reaching the end marker is not the same as the bytes
            // before it being a whole stream: a chase decoder stopped in the
            // middle of a chunk - it ran out of the caller's output budget
            // before the chunk's declared size was produced - is at the end
            // marker with a chunk still owing output, and that stream is
            // truncated. Only the chase can be in that position; a worker
            // either decodes its whole run or fails it.
            && self.st.as_ref().is_none_or(Lzma2Decoder::at_chunk_boundary)
        {
            self.complete = true;
            self.cursor_in = end;
        }
    }
}
