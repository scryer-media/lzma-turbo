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

/// The least a caller's piece is counted as when the bound and the input
/// budget are reckoned in pieces: the stream's own piece size governs, and
/// this only keeps a stream fed in tiny pieces from being bound to a few of
/// them.
const MIN_PIECE: u64 = 64 << 10;

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
    /// With [`Lzma2AdaptiveDecoder::set_hand_back_waits`] on, it also means
    /// a worker is decoding and nothing else could be done without waiting.
    Progress,
    /// The stream's end marker was reached and every block has been delivered.
    Finished,
}

/// Where the decoder's memory is, and why it last held work back: what a
/// caller tuning its read-ahead, or a harness explaining a decode, reads to
/// see the mechanism rather than guess at it.
///
/// Every byte figure is capacity, as in
/// [`held_bytes`](Lzma2AdaptiveDecoder::held_bytes), and the four that split
/// it add up to it. A snapshot: it is taken by
/// [`ledger`](Lzma2AdaptiveDecoder::ledger) and does not move afterwards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AdaptiveLedger {
    /// Input pieces held: unclaimed, and claimed but still read by a worker.
    pub input_bytes: u64,
    /// The buffers of the runs out with workers.
    pub runs_out_bytes: u64,
    /// The decoded blocks waiting to be handed over.
    pub runs_waiting_bytes: u64,
    /// Output buffers and input pieces parked for reuse.
    pub parked_bytes: u64,
    /// The most [`held_bytes`](Lzma2AdaptiveDecoder::held_bytes) has been.
    pub peak_held_bytes: u64,
    /// Runs handed to a worker and not yet taken back: being decoded, or
    /// decoded and not yet collected.
    pub runs_out: usize,
    /// Runs a worker is decoding right now: [`Self::runs_out`] less the ones
    /// no worker has taken yet and the ones whose blocks are finished and
    /// waiting to be collected.
    pub runs_decoding: usize,
    /// Decoded blocks waiting their turn to be handed over.
    pub runs_waiting: usize,
    /// Runs held back at least once because every worker had one.
    pub refused_busy: u64,
    /// Runs held back at least once because there was no room for their
    /// buffer under the limit, while other runs were out.
    pub refused_room: u64,
    /// Runs decoded on the calling thread because there was no room for
    /// them and nothing out whose landing could make some.
    pub refused_room_chase: u64,
    /// Runs decoded on the calling thread because they were too large for
    /// the limit at all.
    pub refused_too_large: u64,
    /// Times there was no complete run at the cursor to dispatch.
    pub refused_incomplete: u64,
    /// Times a piece of input was refused, in whole or in part.
    pub input_refusals: u64,
    /// Times parked capacity was given back to let a run through.
    pub sheds: u64,
}

/// Why a run was held back, for counting each run once per cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Busy,
    Room,
    RoomChase,
    TooLarge,
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

/// Records one diagnostic counter, and nothing at all unless the counters are
/// built. Every use of `Stats` goes through it, so a default build carries
/// neither the arithmetic nor the fields it would write to.
macro_rules! stat {
    ($($t:tt)*) => {
        #[cfg(feature = "adaptive-stats")]
        {
            $($t)*
        }
    };
}

/// Diagnostic counters, so that a regression can be attributed to a mechanism
/// rather than guessed at. Behind `adaptive-stats`, which is not a default
/// feature: they are for a decoder under a profiler, not for the decoder a
/// program ships. With the feature on they are printed when the decoder is
/// dropped and `LZMA_ADAPTIVE_STATS` is set in the environment.
#[cfg(feature = "adaptive-stats")]
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
    shed: u64,
    max_outstanding: u64,
    in_reused: u64,
    in_fresh: u64,
    in_grown: u64,
}

/// An LZMA2 decoder that is fed input and switches between single- and
/// multi-threaded decoding while it runs.
pub struct Lzma2AdaptiveDecoder {
    #[cfg(feature = "adaptive-stats")]
    stats: Stats,
    dict_prop: u8,
    memory_limit: u64,
    threads: usize,
    /// The threads the caller reads ahead for. See
    /// [`Lzma2AdaptiveDecoder::set_read_ahead`].
    read_ahead: usize,
    /// Whether a drain hands control back instead of waiting for a worker.
    /// See [`Lzma2AdaptiveDecoder::set_hand_back_waits`].
    hand_back_waits: bool,
    ordered: bool,
    chase: bool,
    /// Whether the last input offered was refused outright and nothing has
    /// freed room since. A decoder that has refused input is not waiting for
    /// it.
    input_refused: bool,

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
    /// Where the last run the chase finished ends: the start of the next run,
    /// which the chase is not in when it stops there.
    st_retired_end: u64,

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
    /// The packed length of the runs out with workers: their input, which the
    /// queue goes on charging for until each one lands.
    outstanding_packed: u64,
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

    /// The counters [`Lzma2AdaptiveDecoder::ledger`] reports.
    ledger: AdaptiveLedger,
    /// The run last held back and every cause it has been held back for, one
    /// bit per [`Refusal`], so a run refused on every pass of a drain is
    /// counted once per cause and not once per pass - nor again when a cause
    /// comes back after another, as it does when the limit changes mid-run.
    last_refusal: Option<(u64, u8)>,

    /// A worker's error, held until everything before it has been delivered.
    failed: Option<(u64, Error)>,
    complete: bool,
    cancelled: bool,
}

#[cfg(feature = "adaptive-stats")]
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
            #[cfg(feature = "adaptive-stats")]
            stats: Stats::default(),
            dict_prop,
            memory_limit: options.memory_limit,
            threads: options.threads.max(1),
            read_ahead: 0,
            hand_back_waits: false,
            ordered: true,
            chase: true,
            input_refused: false,
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
            st_retired_end: 0,
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
            outstanding_packed: 0,
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
            ledger: AdaptiveLedger::default(),
            last_refusal: None,
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

    /// Sets the threads the caller is reading ahead for, which may be more
    /// than the ceiling [`Lzma2AdaptiveDecoder::set_threads`] put in force: a
    /// caller that widens the decode as complete runs pile up in front of it
    /// has to be able to hand those runs over before it widens. Zero, the
    /// default, is the ceiling in force.
    ///
    /// The decoder works to a run pair per thread and refuses input past that
    /// (see [`Lzma2AdaptiveDecoder::memory_limit`]). For each thread read ahead
    /// for beyond the ceiling it keeps room for one more run's input as well,
    /// and nothing more: a run waiting for a worker holds its input and no
    /// output, and a wider ceiling, once set, counts that thread's pair in
    /// full. Without it, a caller widening on its backlog never has more than
    /// one run beyond the threads in force to show for its reading ahead, and
    /// widens a thread at a time on a stream of large runs. Nothing is
    /// dispatched beyond the ceiling, and the caller's own limit, which the
    /// pair bound never exceeds, is unchanged.
    pub fn set_read_ahead(&mut self, threads: usize) {
        self.read_ahead = threads;
    }

    /// Sets whether a drain hands control back rather than waiting for a
    /// worker. Off by default.
    ///
    /// A drain waits for a worker when there is nothing else it can do: the
    /// input is over, or the decoder is holding all its limit allows. A
    /// caller that may widen the decode while it waits cannot be heard
    /// there, because [`Lzma2AdaptiveDecoder::set_threads`] needs the
    /// decoder back: a wider ceiling is applied only once a run lands, and
    /// on a stream of large runs that is a whole run's decode at the old
    /// width for every widening. With this on, such a drain returns
    /// [`DrainStatus::Progress`] instead, having delivered whatever it could,
    /// and the caller waits where it can also listen, finishing with
    /// [`Lzma2AdaptiveDecoder::wait_for_worker`] when it has nothing to
    /// listen for. What is decoded, and in what order, is unchanged.
    pub fn set_hand_back_waits(&mut self, on: bool) {
        self.hand_back_waits = on;
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

    /// Where the memory is and why work was held back: see
    /// [`AdaptiveLedger`].
    ///
    /// The refusal counts are of runs, each counted once per cause however
    /// many times a drain found it still refused; the counts of incomplete
    /// runs and of refused input are of the occasions, because there is no
    /// run to count them against.
    #[must_use]
    pub fn ledger(&self) -> AdaptiveLedger {
        let parked = self.spare_cap + self.segs.spare_bytes();
        AdaptiveLedger {
            input_bytes: self.segs.held_bytes() - self.segs.spare_bytes(),
            runs_out_bytes: self.outstanding_bytes,
            runs_waiting_bytes: self.ready_cap,
            parked_bytes: parked,
            peak_held_bytes: self.ledger.peak_held_bytes.max(self.held_bytes()),
            runs_out: self.outstanding,
            runs_decoding: self.pool.as_ref().map_or(0, Pool::decoding),
            runs_waiting: self.ready.len() + usize::from(self.part.is_some()),
            ..self.ledger
        }
    }

    /// Raises the peak to what is held now. Called wherever holding can grow.
    fn mark_peak(&mut self) {
        let held = self.held_bytes();
        if held > self.ledger.peak_held_bytes {
            self.ledger.peak_held_bytes = held;
        }
    }

    /// Counts the run at the cursor as held back for `why`, once.
    fn refuse(&mut self, why: Refusal) {
        let bit = 1u8 << why as u8;
        let seen = match self.last_refusal {
            Some((run, causes)) if run == self.next_index => causes,
            _ => 0,
        };
        if seen & bit != 0 {
            return;
        }
        self.last_refusal = Some((self.next_index, seen | bit));
        let l = &mut self.ledger;
        match why {
            Refusal::Busy => l.refused_busy += 1,
            Refusal::Room => l.refused_room += 1,
            Refusal::RoomChase => l.refused_room_chase += 1,
            Refusal::TooLarge => l.refused_too_large += 1,
        }
    }

    /// Samples the accounting for the diagnostic counters.
    #[cfg(feature = "adaptive-stats")]
    fn note_peak(&mut self) {
        self.mark_peak();
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

    /// Samples the accounting for the diagnostic counters. Built away with
    /// them.
    #[cfg(not(feature = "adaptive-stats"))]
    fn note_peak(&mut self) {
        self.mark_peak();
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
    ///
    /// Nor does it include anything the caller is holding: input it has read
    /// and not yet fed, a piece the decoder refused and handed back, output it
    /// has been handed and not yet written. A caller that keeps a queue of its
    /// own and wants one limit over both counts that queue itself, and tells
    /// the decoder what is left with
    /// [`set_memory_limit`](Lzma2AdaptiveDecoder::set_memory_limit).
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

    /// The limit [`Lzma2AdaptiveDecoder::held_bytes`] is kept under: the one
    /// the caller set, or one run pair per thread and two of the caller's
    /// pieces, whichever is the less.
    ///
    /// The second is the decoder's own, and it is what 7-Zip holds, give or
    /// take the pieces the input is held in: a thread
    /// decoding a run has that run's input and the buffer it decodes into,
    /// and a decoder with more runs than threads gains nothing from holding
    /// more than that. Capacity above it is not read-ahead the workers ever
    /// get to; it is input read early and output parked. So once the runs of
    /// the stream are known, the decoder works to that figure under any
    /// larger limit, an unlimited one included, and this reports it.
    #[must_use]
    pub fn memory_limit(&self) -> u64 {
        self.limit()
    }

    /// The limit the decoder works to: the caller's, or the pair bound.
    fn limit(&self) -> u64 {
        self.memory_limit.min(self.pair_bound())
    }

    /// One run pair - its input and its output - for every thread, and one or
    /// two pieces. No bound at all before a run has been seen, because there is
    /// no pair to count yet; the scan reserve governs that stretch.
    ///
    /// The pieces are what holding input in the caller's pieces costs over
    /// holding it in a buffer a run long, which is what 7-Zip does. The runs
    /// out are held in whole pieces, and the span they make does not start or
    /// end on a piece boundary: the piece the first of them starts in has the
    /// end of the run before it, which is a piece at most, and the piece the
    /// caller is handing over runs past the last of them into the next.
    ///
    /// The piece is the size the caller reads in, not the last piece: the
    /// last piece of a stream is whatever was left over, and a bound that
    /// fell with it would put a decoder holding a pair per thread over its
    /// own limit with the last run in hand, which would then wait for a
    /// worker to land.
    ///
    /// A pair is never counted as less than two of the caller's pieces: on a
    /// stream of small runs the bound would otherwise be a few hundred
    /// kilobytes, which saves nothing worth having and starves the pipeline of
    /// the pieces it is fed in. The floor is the caller's piece and not a
    /// fixed size: a thread whose run sits inside one piece still holds that
    /// whole piece, so a pair counted at less than a piece in and a piece out
    /// refuses the piece the next run is in while every thread has work, and
    /// a caller reading in small pieces is held to what its pieces cost
    /// rather than to a figure written for large ones. The slack is the same
    /// piece, so a caller reading in 256 KiB pieces, as 7-Zip reads, is
    /// allowed the 256 KiB of overrun 7-Zip keeps and not a mebibyte.
    fn pair_bound(&self) -> u64 {
        let (run_in, run_out) = self.run_pair();
        if run_in == 0 && run_out == 0 {
            return u64::MAX;
        }
        let piece = self.segs.piece_size().max(MIN_PIECE);
        let pair = run_in.saturating_add(run_out).max(piece.saturating_mul(2));
        // The runs already handed out count at their own sizes, and a run
        // landed and waiting its turn holds its slot's output; only the slots
        // with neither are reckoned at the run in hand. Otherwise a stream
        // whose runs vary would have the bound follow the last, small run
        // down under a large one still out, and the decoder would find itself
        // over its own limit for work it had already given away.
        let landed = self.ready.len() + usize::from(self.part.is_some());
        let idle = self.threads.saturating_sub(self.outstanding + landed);
        let in_hand = self
            .outstanding_packed
            .saturating_add(self.outstanding_bytes)
            .saturating_add(self.ready_cap)
            .saturating_add(pair.saturating_mul(idle as u64));
        // Runs no larger than a piece already count a whole piece of input
        // each, which covers the piece shared with the run before; what is
        // left is the one piece the caller is handing over, as 7-Zip keeps one
        // block's worth of overrun past the runs in hand. Larger runs are held
        // in a span that starts and ends inside a piece, which costs both.
        let slack = if run_in <= piece {
            piece
        } else {
            piece.saturating_mul(2)
        };
        // A thread read ahead for beyond the ceiling holds a run's input
        // until the ceiling reaches it; see `set_read_ahead`.
        let ahead = self.read_ahead.saturating_sub(self.threads) as u64;
        pair.saturating_mul(self.threads as u64)
            .saturating_add(run_in.saturating_mul(ahead))
            .max(in_hand)
            .saturating_add(slack)
    }

    /// The packed and unpacked size of the run the decoder is reckoning with:
    /// the largest of the runs in the backlog the threads will take next, or
    /// the last one dispatched if that was larger.
    ///
    /// Not the front of the backlog alone: a small run at the front of large
    /// ones would have the bound fall under the input already held for the
    /// large ones behind it, and the decoder would refuse to dispatch any of
    /// them for want of room it had already given away.
    fn run_pair(&self) -> (u64, u64) {
        self.pending.iter().take(self.threads).fold(
            (self.last_packed, self.last_unpacked),
            |(run_in, run_out), r| (run_in.max(r.packed_len), run_out.max(r.unpacked_len)),
        )
    }

    /// Changes the limit [`Lzma2AdaptiveDecoder::held_bytes`] is kept under,
    /// from the next feed or dispatch onwards.
    ///
    /// For a caller whose own queue shares one budget with the decoder: it
    /// sets the decoder's limit to the budget less what its queue holds before
    /// each [`feed`](Lzma2AdaptiveDecoder::feed) or
    /// [`drain`](Lzma2AdaptiveDecoder::drain). Lowering it recalls nothing -
    /// runs already out stay out and input already taken stays taken - so
    /// [`held_bytes`](Lzma2AdaptiveDecoder::held_bytes) can sit above a
    /// lowered limit until those are done with. In the meantime input is
    /// refused and no run is dispatched, except that a run that no longer
    /// fits the limit at all is decoded on the calling thread rather than
    /// waited on, as it would have been under that limit from the start.
    pub fn set_memory_limit(&mut self, limit: u64) {
        self.memory_limit = limit;
    }

    /// What dispatching a run of `unpacked_len` bytes to a worker would add to
    /// [`held_bytes`](Lzma2AdaptiveDecoder::held_bytes): the buffer the run is
    /// decoded into, less the parked buffer the dispatch will reuse.
    ///
    /// The decoder dispatches the run at the cursor when `held_bytes()` plus
    /// this is at most [`memory_limit`](Lzma2AdaptiveDecoder::memory_limit),
    /// so a caller gating its own reads on the same arithmetic agrees with it
    /// about when the next run can go. Before it refuses, the decoder also
    /// gives back parked capacity this dispatch would not reuse, so it can
    /// dispatch where this predicate alone says no; it never dispatches where
    /// the predicate, reckoned after that, says no.
    #[must_use]
    pub fn dispatch_cost(&self, unpacked_len: u64) -> u64 {
        unpacked_len.saturating_sub(self.spare_out.last().map_or(0, |b| b.capacity() as u64))
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
        let spare = self.segs.take_spare();
        stat!(if spare.is_some() {
            self.stats.in_reused += 1;
        } else {
            self.stats.in_fresh += 1;
        });
        let mut buf = spare.unwrap_or_default();
        buf.clear();
        // In pieces of a size that repeats. A copy sized to whatever room
        // happens to be free makes every piece a different size, so a parked
        // one rarely fits the next and the allocator maps and unmaps a large
        // region per piece; a steady size means the same few allocations serve
        // the whole stream. A buffer already in hand is used to its capacity,
        // whatever that is - it is mapped either way.
        let take = take.min(PIECE_TARGET.max(buf.capacity()));
        stat!(self.stats.in_grown += u64::from(buf.capacity() < take););
        buf.try_reserve_exact(take).map_err(|_| Error::Alloc)?;
        buf.extend_from_slice(&data[..take]);
        self.segs.push_owned(buf);
        self.fit_parked_input();
        self.mark_peak();
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
    /// The piece costs [`held_bytes`](Lzma2AdaptiveDecoder::held_bytes) its
    /// allocation's capacity, not its length, and is admitted by that: a short
    /// piece in a large buffer is refused where its bytes alone would fit. A
    /// caller cutting a read short should copy a small piece into a buffer its
    /// size, or lend it with
    /// [`feed_shared`](Lzma2AdaptiveDecoder::feed_shared).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`] after [`Lzma2AdaptiveDecoder::cancel`].
    pub fn feed_owned(&mut self, seg: Vec<u8>) -> Result<Option<Vec<u8>>, Error> {
        if self.cancelled {
            return Err(Error::Cancelled);
        }
        if seg.is_empty() {
            return Ok(None);
        }
        // Admitted by what holding it costs, which is the allocation and not
        // the bytes in it: that is what the queue charges, so a short piece in
        // a large buffer checked by its length would be taken under a limit
        // that has no room for it.
        let charge = seg.capacity();
        if self.room_to_take(charge, true)? < charge {
            return Ok(Some(seg));
        }
        self.segs.push_owned(seg);
        self.fit_parked_input();
        self.mark_peak();
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
        self.fit_parked_input();
        self.mark_peak();
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
        if whole && take < len && self.short_of_a_run() {
            // The decoder is short of the run in hand: there is no complete
            // run at the cursor, and what it holds unclaimed is no more than
            // one run. The piece is what completes it, and the floor - the run
            // and a header - is only enough for it when the pieces happen to
            // end near the run's end. A piece that runs on into the next run
            // is refused by the floor, and under a tight limit the floor is
            // all the budget there is, so the caller is left holding the piece
            // the decode is waiting on. Take it whenever the limit itself has
            // room. This cannot read ahead without bound: once taken, the
            // decoder holds more than a run unclaimed and this stops applying
            // until the run is claimed.
            let len = len as u64;
            let free = self.input_free();
            if held.saturating_add(len) <= free {
                take = usize::try_from(len).unwrap_or(usize::MAX);
            }
        }
        // Refused outright, that is: a part taken is input the decoder is
        // still reading, and the rest is offered again on the next turn.
        self.input_refused = take == 0 || (whole && take < len);
        if take < len {
            self.ledger.input_refusals += 1;
        }
        if whole && take < len {
            return Ok(0);
        }
        Ok(take)
    }

    /// Lets go of parked input if keeping it would hold more than the limit.
    ///
    /// The room a piece is admitted into does not count parked input, because
    /// a copy goes into a parked buffer rather than a new one. A piece handed
    /// over whole does not - it is the caller's own buffer - and a copy takes
    /// one parked buffer of however many there are, so once the piece is in,
    /// what is still parked is held on top of it. It is spare capacity and
    /// nothing else, and as much of it as would take the decoder past the
    /// limit goes; the rest is what the next pieces are read into.
    fn fit_parked_input(&mut self) {
        let over = self.held_bytes().saturating_sub(self.limit());
        if self.segs.spare_bytes() > 0 && over > 0 {
            self.segs.shed_spare_down(over);
        }
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

    /// Whether the decoder is waiting on the rest of the run in hand: no
    /// complete run at the cursor, and no more than one run's worth of input
    /// held unclaimed. What it holds and cannot decode - the front of the
    /// piece the cursor is in, the pieces workers still read - is not counted:
    /// it is not the run in hand.
    fn short_of_a_run(&self) -> bool {
        let run = self.known_run();
        let at_cursor = self
            .pending
            .front()
            .is_some_and(|r| r.in_offset == self.cursor_in);
        run != 0 && !at_cursor && self.segs.len() <= run
    }

    /// What the limit leaves for input once everything else the decoder is
    /// holding is counted.
    fn input_free(&self) -> u64 {
        let other = self.held_bytes() - self.segs.held_bytes();
        self.limit().saturating_sub(other)
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

    /// The input's share of the limit, setting aside an output buffer for
    /// every slot that has none.
    ///
    /// A slot is a thread's run pair: the run's input, and the buffer it is
    /// decoded into. A slot with a run out has both, charged where they sit;
    /// a slot whose run has landed and is waiting its turn to be handed over
    /// still has its output, and that buffer is the one its next run will be
    /// decoded into once it is handed over and parked. Neither needs anything
    /// set aside. Only a slot with no output at all does, and for it the
    /// budget sets aside a buffer, not a pair: the input half of that slot's
    /// pair is exactly what this budget is for.
    ///
    /// Setting aside a whole pair for every thread, busy or not, counted the
    /// runs out twice and the read-ahead for the next run as well, and with
    /// most of the threads busy that left the input no share at all.
    /// Treating a slot whose output was waiting its turn as idle was the
    /// same mistake by one buffer: it held back the input for that slot's
    /// next run until the block was handed over, and a slot that is waiting
    /// on the input for its next run is a thread doing nothing.
    fn buf_budget(&self) -> u64 {
        let landed = self.ready.len() + usize::from(self.part.is_some());
        let idle = self
            .threads
            .saturating_sub(self.outstanding)
            .saturating_sub(landed);
        self.buf_budget_for(idle as u64)
    }

    /// The input's share of the limit when `slots` slots have no output
    /// buffer yet.
    fn buf_budget_for(&self, slots: u64) -> u64 {
        let free = self.input_free();
        let (run_in, run_out) = self.run_pair();
        // What the slots without a buffer will want once their runs are
        // dispatched, less what is parked for them already: parked output is
        // charged where it sits, and dispatch moves it rather than allocating.
        let reserve = run_out.saturating_mul(slots).saturating_sub(self.spare_cap);
        // Nothing is known about the runs yet: the scan reserve says how much
        // input to accept against that, and it is what grows when the scan
        // needs more. See `room_to_take`.
        let unknown = run_in == 0 && run_out == 0;
        // A run's packed bytes are not quite enough to claim it: the scanner
        // has to see where the next run starts before it will say the last one
        // is complete, so the floor carries a header's worth beyond it. Input
        // comes in the caller's pieces, so a header's worth is one piece. A
        // fixed mebibyte here was a mebibyte of read-ahead over a bound whose
        // slack is a 256 KiB piece: the input took the room the next run's
        // output needed, and a two-thread decode of 1 MiB runs waited on its
        // workers with a complete run in hand for half its wall time.
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
            .saturating_add(self.segs.piece_size().max(MIN_PIECE));
        // Once the runs are known the limit is at most a pair per thread (see
        // `pair_bound`), so what the outputs are not owed is the input's: a
        // pair per thread is in plus out, and the outs are counted. Before
        // they are known the limit may be anything, and the scan reserve is
        // kept to half of what is free so that finding the first run cannot
        // take the room the first dispatch needs.
        let share = if unknown {
            self.scan_reserve.min(free / 2)
        } else {
            free.saturating_sub(reserve)
        };
        share.max(floor).min(free)
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
        self.outstanding_packed = 0;
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
            if did {
                // A block landed or was handed over, and either one frees
                // room: input refused before it may well be taken now.
                self.input_refused = false;
            }
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
                    stat!(self.stats.chase_steps += 1;);
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
                    //
                    // Nor is it waiting for input it has refused when nothing
                    // has freed room since. Before the first run is known the
                    // input budget is a guess, and a run larger than the limit
                    // is refused piece by piece until its end is seen, which
                    // with nothing out is never; the chase decodes what is
                    // held so the rest can come in.
                    //
                    // Parked input is not in the way: the next piece is
                    // copied into it, and `room_to_take` leaves it out for
                    // that reason. Counting it here would call a decoder full
                    // that has a whole piece's room waiting for the input.
                    let waiting_for_input = !self.chase
                        && !self.input_done
                        && !self.input_refused
                        && self.held_bytes() - self.segs.spare_bytes() < self.limit();
                    if !busy && !waiting_for_input {
                        stat!(self.stats.chase_steps_none_arm += 1;);
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
                // Nothing more can be fed and no run is left to decode, so
                // what is parked for the next piece or the next run never
                // will be used.
                self.segs.shed_spare();
                self.spare_out.clear();
                self.spare_cap = 0;
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
                if !self.input_done && self.held_bytes() < self.limit() {
                    progress |= did;
                    return Ok(if progress {
                        DrainStatus::Progress
                    } else {
                        DrainStatus::NeedsMoreInput
                    });
                }
                // Not more input: the caller waits instead, where it can
                // also widen. See `set_hand_back_waits`.
                if self.hand_back_waits {
                    return Ok(DrainStatus::Progress);
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
        // be dispatching with. Within that, as much as a run's input: a run
        // landing lets go of that many pieces at once and the next run is
        // read into as many, as 7-Zip reads each block into a thread's chain
        // of links kept from the block before. Kept to a few pieces, the rest
        // went back to the allocator and the next run faulted its input in
        // afresh.
        let piece = self.segs.piece_size().max(MIN_PIECE);
        let (run_in, _) = self.run_pair();
        let room = (self.limit() / 8).min(
            piece
                .saturating_mul(self.threads as u64 + 2)
                .max(run_in.saturating_add(piece)),
        );
        let slots = usize::try_from(room / piece)
            .unwrap_or(usize::MAX)
            .max(self.threads + 2);
        self.segs.set_park_budget(room, slots);
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
        // The job's references are exactly the run's packed bytes.
        let packed: u64 = d.packed.iter().map(|s| s.bytes().len() as u64).sum();
        self.outstanding_packed -= packed;
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
                self.mark_peak();
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
        // A run buffer is parked only for a thread that has no output buffer:
        // one with a run out has one, and so does one whose run has landed
        // and is waiting its turn, which will come back here before that
        // thread can be given another run. A buffer parked beyond those is
        // capacity the limit counts and nothing can use, and at a pair per
        // thread it is the input of the next run, which then cannot be read
        // until a worker lands. Chase-step buffers are small and come and go
        // with the chase, so they are left to the cap below.
        let landed = self.ready.len() + usize::from(self.part.is_some());
        let surplus =
            buf.capacity() > OUT_STEP_ST && self.outstanding + landed + held >= self.threads;
        if !surplus && held < self.threads + 2 && self.worth_parking(buf.capacity(), want, held) {
            stat!(self.stats.parked_out += 1;);
            self.spare_cap += buf.capacity() as u64;
            self.spare_out.push(buf);
        } else {
            stat!(self.stats.dropped_out += 1;);
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
        let cap_room = (self.limit() / 8).min(
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
            stat!(self.stats.chase_threads1 += 1;);
            return Ok(Dispatch::Chase);
        }
        if self.st_in_run {
            stat!(self.stats.chase_st_in_run += 1;);
            return Ok(Dispatch::Chase);
        }
        let Some(run) = self.pending.front().copied() else {
            stat!(self.stats.none += 1;);
            self.ledger.refused_incomplete += 1;
            return Ok(Dispatch::None);
        };
        if run.in_offset != self.cursor_in {
            stat!(self.stats.none += 1;);
            self.ledger.refused_incomplete += 1;
            return Ok(Dispatch::None);
        }
        if self.outstanding >= self.threads {
            stat!(self.stats.busy += 1;);
            self.refuse(Refusal::Busy);
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
        if size > self.limit() {
            stat!(self.stats.chase_too_big += 1;);
            self.refuse(Refusal::TooLarge);
            return Ok(Dispatch::Chase);
        }
        if !self.room_for(run) && self.shed_for(run) {
            stat!(self.stats.shed += 1;);
            self.ledger.sheds += 1;
        }
        if !self.room_for(run) {
            // Room appears when an outstanding block lands. If none is
            // outstanding there is nothing to wait for, so the chase decoder
            // takes it and streams it instead.
            stat!(self.stats.refused_held += self.held_bytes(););
            return Ok(if self.outstanding == 0 {
                stat!(self.stats.chase_no_room += 1;);
                self.refuse(Refusal::RoomChase);
                Dispatch::Chase
            } else {
                stat!(self.stats.busy += 1;);
                self.refuse(Refusal::Room);
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
            stat!(self.stats.chase_pool_empty += 1;);
            return Ok(Dispatch::Chase);
        }

        let mut out = self.take_out();
        // The buffer is sized here, on the thread that will free it, rather
        // than grown by the worker. The allocator keeps the memory a thread
        // frees in that thread's own arena when another thread allocated it,
        // so a buffer grown on a worker and dropped here leaves its pages
        // behind in the worker's arena, which the limit does not see and
        // nothing reuses. The worker still writes every page, so this moves
        // the allocation and not the cost of faulting the buffer in.
        if out.capacity() < unpacked {
            out = Vec::new();
            out.try_reserve_exact(unpacked).map_err(|_| Error::Alloc)?;
        }
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

        stat!(self.stats.sent += 1;);
        stat!(self.stats.worker_bytes += run.unpacked_len;);
        self.outstanding_unpacked += run.unpacked_len;
        self.outstanding_packed += run.packed_len;
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
        self.mark_peak();
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
        self.held_bytes() + self.dispatch_cost(run.unpacked_len) <= self.limit()
    }

    /// Gives back the parked capacity a dispatch of `run` would not reuse, if
    /// that is what stands between it and the limit. Returns whether anything
    /// was given back.
    ///
    /// Parking is for saving an allocation and a page fault per page; it is
    /// not worth a refused dispatch, which costs a worker its whole run. The
    /// dispatch reuses the last output buffer parked and no input buffer at
    /// all, so every other output buffer and every parked input piece is
    /// capacity standing in the run's way. It goes only when going is enough
    /// to let the run through: a dispatch that would be refused anyway keeps
    /// its pools for the runs after it.
    fn shed_for(&mut self, run: Lzma2Run) -> bool {
        let keep = self.spare_out.last().map_or(0, |b| b.capacity() as u64);
        let unused = self.spare_cap - keep + self.segs.spare_bytes();
        if unused == 0 {
            return false;
        }
        let after = self.held_bytes() - unused;
        if after.saturating_add(self.dispatch_cost(run.unpacked_len)) > self.limit() {
            return false;
        }
        // Over the limit by this much with everything parked still held.
        let over = self
            .held_bytes()
            .saturating_add(self.dispatch_cost(run.unpacked_len))
            .saturating_sub(self.limit());
        if let Some(last) = self.spare_out.pop() {
            self.spare_out.clear();
            self.spare_out.push(last);
        }
        let outputs = self.spare_cap - keep;
        self.spare_cap = keep;
        // Parked input goes only as far as the run needs: the rest is what
        // the next pieces are read into.
        self.segs.shed_spare_down(over.saturating_sub(outputs));
        true
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
        self.mark_peak();

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
                self.st_retired_end = front.in_offset + front.packed_len;
                self.pending.pop_front();
                self.runs_claimed += 1;
                self.next_index += 1;
            } else {
                break;
            }
        }
        self.st_in_run = match self.pending.front() {
            Some(front) => self.cursor_in > front.in_offset,
            // Past every complete run: inside the run still arriving, at its
            // start, or exactly at the end marker. At its start - the end of
            // the run the chase has just finished - the chase is in no run,
            // and the next one is a worker's once it is complete; holding on
            // there would decode it here a step at a time, chasing off or
            // not.
            None => {
                let at_start = self.cursor_in == self.st_retired_end
                    && self.st.as_ref().is_none_or(Lzma2Decoder::at_chunk_boundary);
                !at_start && self.stream_end.is_none_or(|e| self.cursor_in + 1 < e)
            }
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

#[cfg(test)]
mod tests {
    //! The admission and dispatch rules, driven by hand.
    //!
    //! These call `scan` and `dispatch` directly and never `collect` unless the
    //! test says so, so the runs out with workers and the input they pin stay
    //! exactly where the test put them whatever the workers get on with: a
    //! worker hands its claim back inside the block it finishes, and nothing
    //! here takes that block in until it is asked to.

    use super::*;

    const MIB: usize = 1 << 20;

    /// A run of LZMA2 stored chunks unpacking to `len` bytes: `0x01` for the
    /// first chunk, `0x02` after, at most 64 KiB a chunk.
    fn run(len: usize, seed: u8) -> Vec<u8> {
        let mut packed = Vec::new();
        let mut left = len;
        let mut first = true;
        while left > 0 {
            let n = left.min(1 << 16);
            packed.push(if first { 0x01 } else { 0x02 });
            let size = u16::try_from(n - 1).expect("at most 64 KiB");
            packed.extend_from_slice(&size.to_be_bytes());
            packed.extend((0..n).map(|i| seed.wrapping_add(i as u8)));
            first = false;
            left -= n;
        }
        packed
    }

    fn decoder(threads: usize, limit: u64) -> Lzma2AdaptiveDecoder {
        Lzma2AdaptiveDecoder::new(
            16,
            &Lzma2MtOptions {
                threads,
                memory_limit: limit,
            },
        )
        .expect("props")
    }

    /// Exactly `bytes`, in an allocation exactly that large, so what the
    /// queue charges for it is what the test reckons with.
    fn exact(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec().into_boxed_slice().into_vec()
    }

    /// Puts a piece in the queue without asking the admission rule: the
    /// state a test starts from, not the thing it tests.
    fn hold(d: &mut Lzma2AdaptiveDecoder, bytes: &[u8]) {
        d.segs.push_owned(exact(bytes));
    }

    #[test]
    fn a_piece_is_reckoned_against_the_idle_slots_only() {
        // Four threads, three runs out, the fourth run in hand and not yet
        // declared: the scanner has not seen where the next one starts.
        let mut d = decoder(4, 50 * MIB as u64);
        for i in 0..4 {
            hold(&mut d, &run(4 * MIB, i));
        }
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 3);
        for _ in 0..3 {
            assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        }
        assert_eq!(d.outstanding, 3);

        // The next piece starts at the boundary. The three runs out are
        // already charged where they sit, so the budget sets aside a run for
        // the one idle slot and no more, and that leaves room for the piece.
        let piece = exact(&run(4 * MIB, 4)[..2 * MIB]);
        let held = d.segs.held_bytes() - d.segs.spare_bytes();
        assert!(
            d.buf_budget() >= held + piece.len() as u64,
            "budget {} for {} held and a {} piece",
            d.buf_budget(),
            held,
            piece.len()
        );
        assert!(d.feed_owned(piece).expect("feed").is_none(), "refused");
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 1, "the fourth run is declared");
    }

    #[test]
    fn the_piece_that_completes_the_run_in_hand_is_taken() {
        // Eight threads under a caller's limit tighter than a pair each, four
        // runs out: the four slots without an output buffer have theirs set
        // aside, and what is left for input is the floor - the run in hand and
        // a header.
        let mut d = decoder(8, 50 * MIB as u64);
        for i in 0..4 {
            hold(&mut d, &run(4 * MIB, i));
        }
        let mut tail = run(4 * MIB, 4);
        tail.extend_from_slice(&run(4 * MIB, 5));
        // Three megabytes of the fifth run, which shows its header and so
        // declares the fourth.
        hold(&mut d, &tail[..3 * MIB]);
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 4);
        for _ in 0..4 {
            assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        }
        assert_eq!(d.outstanding, 4);

        // A whole 4 MiB piece: the megabyte or so that completes the fifth run
        // and three megabytes of the sixth. The floor leaves room for the run
        // and a megabyte beyond it, which is less than the piece, but the
        // limit has room for it.
        let piece = exact(&tail[3 * MIB..7 * MIB]);
        assert!(d.limit() < d.pair_bound());
        assert!(d.feed_owned(piece).expect("feed").is_none(), "refused");
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 1, "the fifth run is declared");
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert!(d.held_bytes() <= d.limit());
    }

    #[test]
    fn a_short_last_piece_does_not_shrink_the_pair_bound() {
        // Three runs and the end marker, fed in whole pieces of the size the
        // caller reads in: the last piece is the few hundred bytes left over.
        let mut stream = run(4 * MIB, 0);
        stream.extend_from_slice(&run(4 * MIB, 1));
        stream.extend_from_slice(&run(4 * MIB, 2));
        stream.push(0);
        let tail = stream.len() % PIECE_TARGET;
        assert!(tail != 0 && (tail as u64) < MIN_BUF_BUDGET);

        let mut d = decoder(2, u64::MAX);
        let (whole, last) = stream.split_at(stream.len() - tail);
        for piece in whole.chunks(PIECE_TARGET) {
            hold(&mut d, piece);
        }
        d.scan().expect("scan");
        let bound = d.pair_bound();
        assert!(bound < u64::MAX);

        // The tail is the end of the stream, not the size of the pieces in
        // flight: the bound a pair per thread and its pieces make is the one
        // the whole pieces made. Were it to fall with the tail, a decoder
        // holding a pair per thread and its whole pieces would find itself
        // over its own limit with the last run in hand, and that run would
        // wait for a worker to land before it could go.
        hold(&mut d, last);
        d.scan().expect("scan");
        assert_eq!(d.pair_bound(), bound);
    }

    #[test]
    fn a_pair_per_thread_held_in_whole_pieces_fits_the_bound() {
        // Two threads, 1 MiB pieces, and runs that do not line up with them:
        // a small first run ends four bytes short of the first piece's end,
        // so the two 4 MiB runs after it are held in pieces reaching from
        // that first piece to one past their own end - a piece more than
        // their packed bytes come to.
        let mut stream = run(MIB - 52, 0);
        assert_eq!(stream.len(), MIB - 4);
        stream.extend_from_slice(&run(4 * MIB, 1));
        stream.extend_from_slice(&run(4 * MIB, 2));
        stream.extend_from_slice(&run(2 * MIB, 3));
        stream.push(0);

        let mut d = decoder(2, u64::MAX);
        for piece in stream[..10 * MIB].chunks(MIB) {
            hold(&mut d, piece);
        }
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 3);

        // The small run goes, lands and is handed over; the first piece stays,
        // because the next run starts in it.
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert!(d.collect(true));
        let mut left = usize::MAX;
        d.emit(&mut |_, _: &[u8]| {}, &mut left);
        assert_eq!(d.segs.held_bytes() - d.segs.spare_bytes(), 10 * MIB as u64);

        // Each thread takes one of the 4 MiB runs. That is a pair per thread,
        // which is what the bound is for, and it must not be refused for the
        // pieces the pairs are held in.
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert!(d.held_bytes() <= d.limit());
    }

    #[test]
    fn no_more_output_buffers_are_kept_than_there_are_threads() {
        let mut stream = run(4 * MIB, 0);
        for i in 1..4 {
            stream.extend_from_slice(&run(4 * MIB, i));
        }
        stream.push(0);
        let mut d = decoder(2, u64::MAX);
        hold(&mut d, &stream);
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 4);

        // Both threads take a run, both land and wait to be handed over, and
        // the third run goes out while they wait: three output buffers for
        // two threads, which is the pipeline working, not waste.
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        while d.outstanding > 0 {
            assert!(d.collect(true));
        }
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert_eq!((d.outstanding, d.ready.len()), (1, 2));

        // The first is handed over. Both threads still have an output buffer -
        // one decoding into it, one waiting its turn with it - so nothing
        // could take a parked one before one of those two came back, and that
        // one is what it would take. Parked, it is capacity the limit counts
        // and nothing uses: at a pair per thread, it is the input of the next
        // run, which then cannot be read until a worker lands.
        let mut left = 4 * MIB;
        d.emit(&mut |_, _: &[u8]| {}, &mut left);
        assert_eq!(d.ready.len(), 1);
        assert!(d.spare_out.is_empty(), "{} parked", d.spare_out.len());

        // The second is handed over with one thread decoding: the other has
        // no buffer, and the next run will want one.
        let mut left = 4 * MIB;
        d.emit(&mut |_, _: &[u8]| {}, &mut left);
        assert_eq!(d.spare_out.len(), 1);
    }

    #[test]
    fn the_ledger_splits_what_is_held_and_counts_each_refusal_once() {
        let mut stream = run(4 * MIB, 0);
        stream.extend_from_slice(&run(4 * MIB, 1));
        stream.extend_from_slice(&run(4 * MIB, 2));
        stream.push(0);
        let mut d = decoder(2, u64::MAX);
        hold(&mut d, &stream);
        d.scan().expect("scan");

        let sums = |d: &Lzma2AdaptiveDecoder| {
            let l = d.ledger();
            assert_eq!(
                l.input_bytes + l.runs_out_bytes + l.runs_waiting_bytes + l.parked_bytes,
                d.held_bytes()
            );
            assert!(l.peak_held_bytes >= d.held_bytes());
            l
        };
        let l = sums(&d);
        assert_eq!(l.input_bytes, stream.len() as u64);
        assert_eq!((l.runs_out, l.runs_waiting), (0, 0));

        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        let l = sums(&d);
        assert_eq!(l.runs_out, 2);
        assert_eq!(l.runs_out_bytes, 8 * MIB as u64);
        assert!(l.runs_decoding <= 2);

        // Both threads busy: the third run is held back, and however often
        // the decoder looks at it, that is one refusal.
        for _ in 0..3 {
            assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Busy);
        }
        assert_eq!(d.ledger().refused_busy, 1);
        assert_eq!(d.ledger().refused_room, 0);

        // Landed and waiting to be handed over: out of the workers' hands,
        // and no longer decoding.
        while d.outstanding > 0 {
            assert!(d.collect(true));
        }
        let l = sums(&d);
        assert_eq!((l.runs_out, l.runs_decoding, l.runs_waiting), (0, 0, 2));
        assert_eq!(l.runs_waiting_bytes, 8 * MIB as u64);
        let peak = l.peak_held_bytes;

        // Handed over: the buffers are parked or gone, and the peak stays.
        let mut left = usize::MAX;
        d.emit(&mut |_, _: &[u8]| {}, &mut left);
        let l = sums(&d);
        assert_eq!((l.runs_waiting, l.runs_waiting_bytes), (0, 0));
        assert_eq!(l.peak_held_bytes, peak);
    }

    #[test]
    fn an_owned_piece_is_admitted_by_what_it_will_cost() {
        // A head of 2304 bytes cut from a 4 MiB read, still in the 4 MiB
        // allocation. Holding it costs the allocation, and a limit with room
        // for the bytes but not the allocation must refuse it.
        let stream = run(64 << 10, 0);
        let mut head = Vec::with_capacity(4 * MIB);
        head.extend_from_slice(&stream[..2304]);
        assert!(head.capacity() >= 4 * MIB);

        let mut d = decoder(2, MIB as u64);
        let back = d.feed_owned(head).expect("feed");
        assert!(
            back.is_some(),
            "a 4 MiB allocation taken under a 1 MiB limit"
        );
        assert!(d.held_bytes() <= d.memory_limit());

        // The same bytes in an allocation their size are taken.
        let exact = exact(&stream[..2304]);
        assert!(d.feed_owned(exact).expect("feed").is_none());
        assert!(d.held_bytes() <= d.memory_limit());
    }

    #[test]
    fn the_pair_bound_does_not_fall_under_a_large_run_out() {
        // A 12 MiB run, a 1 MiB run, and the start of a third the scanner has
        // not seen the end of. Two threads take the first two.
        let mut stream = run(12 * MIB, 0);
        stream.extend_from_slice(&run(MIB, 1));
        let third = run(4 * MIB, 2);
        stream.extend_from_slice(&third[..MIB]);
        let mut d = decoder(2, u64::MAX);
        for piece in stream.chunks(MIB) {
            hold(&mut d, piece);
        }
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 2);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);

        // Nothing is waiting at the cursor, and the last run dispatched is the
        // small one. Reckoned from that alone, a pair per thread is far less
        // than the large run out already holds; the bound counts the runs out
        // at their own sizes, so the decoder is never over its own limit for
        // work it has already handed out.
        assert!(d.pending.is_empty());
        assert!(
            d.held_bytes() <= d.limit(),
            "{} held over a limit of {}",
            d.held_bytes(),
            d.limit()
        );
    }

    #[test]
    fn a_refused_reader_is_never_left_waiting_on_itself() {
        // Chase off, a 2 MiB limit, and a first run of 4 MiB fed in 512 KiB
        // pieces: the run is larger than the limit, and until its end is seen
        // nobody knows that. The decoder takes what the scan reserve allows
        // and then refuses the next piece, with nothing out and no complete
        // run to dispatch.
        let mut stream = run(4 * MIB, 0);
        stream.extend_from_slice(&run(1 << 16, 1));
        stream.push(0);
        let mut d = decoder(4, 2 * MIB as u64);
        d.set_chase(false);

        let mut out = 0usize;
        let mut pieces = stream.chunks(MIB / 2).peekable();
        let mut hand: Option<Vec<u8>> = None;
        loop {
            if hand.is_none() {
                hand = pieces.next().map(exact);
            }
            let refused = match hand.take() {
                Some(p) => {
                    hand = d.feed_owned(p).expect("feed");
                    hand.is_some()
                }
                None => {
                    d.end_of_input();
                    false
                }
            };
            let status = d.drain(|_, b| out += b.len()).expect("drain");
            if status == DrainStatus::Finished {
                break;
            }
            // Asking for more input it has just refused, with nothing out to
            // wait for, is a decoder waiting on its own caller forever. With
            // the input refused, the decode has to go on without it.
            assert!(
                !(refused && status == DrainStatus::NeedsMoreInput && d.outstanding == 0),
                "refused a piece and then asked for more, with nothing in flight"
            );
        }
        assert_eq!(out, 4 * MIB + (1 << 16));
    }

    #[test]
    fn parked_capacity_is_shed_before_a_run_is_refused() {
        let mut d = decoder(4, 100 * MIB as u64);
        for i in 0..3 {
            hold(&mut d, &run(4 * MIB, i));
        }
        // An 8 MiB run, a small one and the end marker, in one piece the size
        // of the 8 MiB run, so the input pieces before it are worth parking.
        let mut rest = run(8 * MIB, 3);
        rest.extend_from_slice(&run(1 << 16, 4));
        rest.push(0);
        hold(&mut d, &rest);
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 5);
        for _ in 0..3 {
            assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        }

        // Let the three runs land and hand them over, so their buffers are
        // parked for the next run and the input they were read from is parked
        // for the next piece.
        while d.outstanding > 0 {
            assert!(d.collect(true));
        }
        let mut left = usize::MAX;
        let mut got = 0usize;
        d.emit(&mut |_, b: &[u8]| got += b.len(), &mut left);
        assert_eq!(got, 12 * MIB);
        assert!(
            d.spare_out.len() >= 2,
            "{} output buffers",
            d.spare_out.len()
        );
        assert!(d.segs.spare_bytes() > 0);

        // A limit the 8 MiB run is refused under by less than the capacity
        // this dispatch will not reuse.
        let next = d.pending[0];
        let reuse = d.spare_out.last().map_or(0, |b| b.capacity() as u64);
        let need = next.unpacked_len - reuse;
        let unused = d.spare_cap - reuse + d.segs.spare_bytes();
        let over = MIB as u64;
        assert!(over < unused);
        d.set_memory_limit(d.held_bytes() + need - over);
        assert!(!d.room_for(next));

        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert_eq!(d.spare_cap, 0);
        assert!(d.spare_out.is_empty());
        // The output buffers let go of cover the run, so the parked input,
        // which the next pieces are read into, is kept.
        assert!(d.segs.spare_bytes() > 0);
        assert!(d.held_bytes() <= d.memory_limit);
    }

    #[test]
    fn a_lowered_limit_governs_the_next_dispatch() {
        let mut d = decoder(4, 100 * MIB as u64);
        let mut stream = run(4 * MIB, 0);
        stream.extend_from_slice(&run(4 * MIB, 1));
        stream.extend_from_slice(&run(4 * MIB, 2));
        stream.push(0);
        hold(&mut d, &stream);
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 3);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);

        // Nothing parked: the run costs its whole buffer.
        let next = d.pending[0];
        assert!(d.spare_out.is_empty());
        assert_eq!(d.dispatch_cost(next.unpacked_len), next.unpacked_len);

        // Mid-stream, the caller's own queue grows and it lowers the limit to
        // what is left: one byte short of the next run, then exactly enough.
        let cost = d.dispatch_cost(next.unpacked_len);
        d.set_memory_limit(d.held_bytes() + cost - 1);
        assert_eq!(d.memory_limit(), d.held_bytes() + cost - 1);
        assert!(!d.room_for(next));
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Busy);
        d.set_memory_limit(d.held_bytes() + cost);
        assert!(d.room_for(next));
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert!(d.held_bytes() <= d.memory_limit());

        // A parked buffer is what the next dispatch reuses, and the cost is
        // what it does not cover - the arithmetic `room_for` refuses on.
        d.recycle(Vec::with_capacity(MIB));
        let last = d.pending[0];
        let reuse = d.spare_out.last().expect("parked").capacity() as u64;
        let cost = d.dispatch_cost(last.unpacked_len);
        assert_eq!(cost, last.unpacked_len - reuse);
        d.set_memory_limit(d.held_bytes() + cost - 1);
        assert!(!d.room_for(last));
        d.set_memory_limit(d.held_bytes() + cost);
        assert!(d.room_for(last));
        assert_eq!(d.dispatch_cost(reuse / 2), 0);
    }

    /// Decodes a whole run on a worker and hands it over, leaving the piece
    /// it came in parked and the start of the next run in hand.
    fn one_run_through_a_worker(d: &mut Lzma2AdaptiveDecoder) -> u64 {
        let first = run(MIB, 0);
        let next = run(MIB, 1);
        let mut out = 0u64;
        assert!(d.feed_owned(exact(&first)).expect("feed").is_none());
        assert!(
            d.feed_owned(exact(&next[..MIB / 2]))
                .expect("feed")
                .is_none()
        );
        while d.runs_claimed() == 0 || d.outstanding != 0 || out < MIB as u64 {
            d.drain(|_, b| out += b.len() as u64).expect("drain");
            d.wait_for_worker();
        }
        out
    }

    #[test]
    fn parked_input_does_not_make_a_decoder_short_of_input_chase() {
        // A decoder with chasing off, the run at the cursor half fed, nothing
        // out: it waits for the rest of the run. The piece the last run came
        // in is parked for the next copy, and the limit is exactly what is
        // held. Without the parked piece there is a piece's room, so this is
        // a decoder short of input and not one at its limit.
        let mut d = decoder(2, u64::MAX);
        d.set_chase(false);
        assert_eq!(one_run_through_a_worker(&mut d), MIB as u64);
        assert!(d.segs.spare_bytes() > 0, "the first run's piece is parked");
        assert_eq!(d.outstanding, 0);
        assert_eq!(d.chase_decoded_bytes(), 0);

        d.set_memory_limit(d.held_bytes());
        let status = d.drain(|_, _| {}).expect("drain");
        assert_eq!(status, DrainStatus::NeedsMoreInput);
        assert_eq!(
            d.chase_decoded_bytes(),
            0,
            "the calling thread decoded a run it was only waiting to be fed"
        );
    }

    #[test]
    fn a_piece_handed_over_does_not_take_held_past_the_limit_by_what_is_parked() {
        // The piece the last run came in is parked for the next copy, and the
        // limit has room for the next piece only if the parked one is not
        // counted. A piece handed over is not copied into the parked one, so
        // taking it and keeping the parked one would hold both.
        let mut d = decoder(2, u64::MAX);
        d.set_chase(false);
        assert_eq!(one_run_through_a_worker(&mut d), MIB as u64);
        let parked = d.segs.spare_bytes();
        assert!(parked > 0, "the first run's piece is parked");

        let next = run(MIB, 1);
        let piece = exact(&next[MIB / 2..]);
        let limit = d.held_bytes() - parked + piece.capacity() as u64;
        d.set_memory_limit(limit);
        assert!(
            d.feed_owned(piece).expect("feed").is_none(),
            "{:?}",
            d.ledger()
        );
        assert!(
            d.held_bytes() <= d.memory_limit(),
            "held {} over the limit {}: {:?}",
            d.held_bytes(),
            d.memory_limit(),
            d.ledger()
        );
    }

    #[test]
    fn a_piece_handed_over_with_runs_out_does_not_take_held_past_the_limit() {
        // The same with work in hand, which is the ordinary budget's path: a
        // run out on a worker, a piece parked, and a limit with room for the
        // next piece only beside the parked one's capacity.
        let mut d = decoder(2, u64::MAX);
        d.set_chase(false);
        assert_eq!(one_run_through_a_worker(&mut d), MIB as u64);
        let parked = d.segs.spare_bytes();
        assert!(parked > 0, "the first run's piece is parked");
        let next = run(MIB, 1);
        let third = run(MIB, 2);
        let mut rest = next[MIB / 2..].to_vec();
        rest.extend_from_slice(&third[..1]);
        assert!(d.feed_owned(exact(&rest)).expect("feed").is_none());
        d.scan().expect("scan");
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert!(d.has_work_in_hand());
        let parked = d.segs.spare_bytes();
        assert!(parked > 0, "a piece is parked: {:?}", d.ledger());

        let piece = exact(&third[1..third.len() / 2]);
        let limit = d.held_bytes() - parked + piece.capacity() as u64;
        d.set_memory_limit(limit);
        assert_eq!(d.memory_limit(), limit, "the pair bound is not the limit");
        assert!(
            d.feed_owned(piece).expect("feed").is_none(),
            "{:?}",
            d.ledger()
        );
        assert!(
            d.held_bytes() <= d.memory_limit(),
            "held {} over the limit {}: {:?}",
            d.held_bytes(),
            d.memory_limit(),
            d.ledger()
        );
    }

    #[test]
    fn the_chase_lets_go_at_the_end_of_a_run() {
        // A run too large for the limit is chased; the next run has only
        // begun to arrive. Once the chase has finished the first run it is
        // between runs, not inside one, and the next run is the workers' as
        // soon as it is complete.
        let mut d = decoder(2, u64::MAX);
        d.set_chase(false);
        let first = run(256 << 10, 0);
        let next = run(256 << 10, 1);
        hold(&mut d, &first);
        hold(&mut d, &next[..next.len() / 2]);
        d.scan().expect("scan");
        assert_eq!(d.pending.len(), 1);

        d.set_memory_limit(1 << 10);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Chase);
        let mut left = usize::MAX;
        while !d.pending.is_empty() {
            assert!(d.st_step(&mut |_, _| {}, &mut left).expect("step"));
        }
        assert_eq!(d.cursor_in, first.len() as u64);
        d.set_memory_limit(u64::MAX);

        assert!(!d.st_in_run, "at the boundary the chase is in no run");
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::None);
    }

    #[test]
    fn an_empty_owned_piece_after_cancel_is_refused_as_cancelled() {
        let mut d = decoder(2, u64::MAX);
        d.cancel();
        assert!(matches!(d.feed_owned(Vec::new()), Err(Error::Cancelled)));
        assert!(matches!(
            d.feed_shared(&Arc::new(Vec::new()), 0..0),
            Err(Error::Cancelled)
        ));
        assert!(matches!(d.feed(&[]), Err(Error::Cancelled)));
    }

    #[test]
    fn a_run_refused_for_a_cause_again_is_not_counted_again() {
        // One run, refused for want of a thread, then for want of room once
        // the caller lowers the limit, then for want of a thread again once
        // it raises it: two causes, each counted once.
        let mut d = decoder(2, u64::MAX);
        let mut stream = Vec::new();
        for i in 0..4 {
            stream.extend_from_slice(&run(MIB, i));
        }
        stream.push(0);
        hold(&mut d, &stream);
        d.scan().expect("scan");
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);

        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Busy);
        d.threads = 3;
        d.set_memory_limit(d.held_bytes());
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Busy);
        d.threads = 2;
        d.set_memory_limit(u64::MAX);
        assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Busy);

        let l = d.ledger();
        assert_eq!(l.refused_busy, 1, "{l:?}");
        assert_eq!(l.refused_room, 1, "{l:?}");
    }

    #[test]
    fn the_next_runs_go_out_while_a_landed_run_waits_to_be_drained() {
        // 7-Zip's in-order thread holds its block until it has written it,
        // and the other threads read and decode the blocks after it in the
        // meantime: with four threads, three runs are taken in and decoded
        // while the first waits for the caller. The same here, under the pair
        // bound: nothing is drained, so the run that landed holds its pair,
        // and the fourth run is still admitted and dispatched.
        let mut d = decoder(4, u64::MAX);
        let runs: Vec<_> = (0..4).map(|i| run(4 * MIB, i)).collect();
        let mut stream = Vec::new();
        let mut cuts = Vec::new();
        for r in &runs {
            stream.extend_from_slice(r);
            cuts.push(stream.len() + 1);
        }
        stream.push(0);
        *cuts.last_mut().expect("four runs") = stream.len();
        let mut from = 0;
        let mut pieces = cuts.iter().map(|&to| {
            let p = exact(&stream[from..to]);
            from = to;
            p
        });

        for _ in 0..3 {
            let piece = pieces.next().expect("piece");
            assert!(d.feed_owned(piece).expect("feed").is_none());
            d.scan().expect("scan");
            assert_eq!(d.dispatch().expect("dispatch"), Dispatch::Sent);
        }
        // One of them lands and is not drained.
        assert!(d.collect(true));
        let l = d.ledger();
        assert!(l.runs_waiting >= 1, "{l:?}");

        let last = pieces.next().expect("piece");
        assert!(
            d.feed_owned(last).expect("feed").is_none(),
            "the fourth run's input refused while a landed run waits: {:?}",
            d.ledger()
        );
        d.scan().expect("scan");
        assert_eq!(
            d.dispatch().expect("dispatch"),
            Dispatch::Sent,
            "{:?}",
            d.ledger()
        );
        let l = d.ledger();
        assert_eq!(l.runs_out + l.runs_waiting, 4, "{l:?}");
        assert!(l.runs_waiting >= 1, "{l:?}");
        assert!(
            d.held_bytes() <= d.pair_bound(),
            "held {} over the pair bound {}",
            d.held_bytes(),
            d.pair_bound()
        );
    }

    #[test]
    fn a_finished_decoder_keeps_nothing_parked() {
        // Nothing can be fed to a finished decoder, so nothing it parked for
        // the next piece or the next run will ever be used.
        let mut d = decoder(2, u64::MAX);
        let mut stream = run(MIB, 0);
        stream.extend_from_slice(&run(MIB, 1));
        stream.push(0);
        let mut pos = 0;
        loop {
            if pos < stream.len() {
                let end = (pos + (256 << 10)).min(stream.len());
                pos += d.feed(&stream[pos..end]).expect("feed");
                if pos == stream.len() {
                    d.end_of_input();
                }
            }
            if d.drain(|_, _| {}).expect("drain") == DrainStatus::Finished {
                break;
            }
            d.wait_for_worker();
        }
        assert_eq!(d.held_bytes(), 0, "{:?}", d.ledger());
    }
}
