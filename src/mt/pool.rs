//! A pool of LZMA2 run decoders that outlives any one decode mode.
//!
//! C: nothing. [`super::mtdec`] is a faithful port of `C/MtDec.c`, in which
//! the threads *are* the control flow: they hand two event tokens around a
//! ring, thread 0 runs on the caller's stack, and the whole structure exists
//! for the duration of one blocking `Lzma2DecMt_Decode` call. That is a good
//! design for "decode this stream, return when done", and it is what the
//! throughput path uses.
//!
//! It cannot serve a caller that is chasing a download: such a caller feeds
//! bytes as they arrive, wants output back without blocking, and switches
//! between single- and multi-threaded decoding as the backlog of complete runs
//! grows and shrinks. So the adaptive decoder drives workers instead of being
//! driven by them, and this is the pool it drives: threads that are spawned on
//! first use, block on a channel when idle, decode whole runs handed to them,
//! and stay parked across a mode change rather than being torn down.
//!
//! The deviation is recorded in `docs/porting.md`.

use alloc::vec::Vec;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;

use crate::error::{Error, FinishMode};
use crate::lzma2::Lzma2Decoder;

#[cfg(feature = "crc")]
use crate::mt::checksum::{BlockChecks, ChecksumPlan, Segmenter};

/// One independently decodable run, on its way to a worker.
pub(crate) struct Job {
    /// The run's index in the stream.
    pub(crate) index: u64,
    /// Where the run's output belongs.
    pub(crate) out_offset: u64,
    /// What the run's chunk headers say it decodes to.
    pub(crate) unpacked_len: usize,
    /// The run's compressed bytes, exactly: the next run's control byte is not
    /// included.
    pub(crate) packed: Vec<u8>,
    /// A buffer to decode into, recycled from a previous block.
    pub(crate) out: Vec<u8>,
    /// What the dispatcher charged its memory accounting for this job: the two
    /// buffers above, as they were when they left. Carried by the job and
    /// handed back untouched so that what comes off the running total is
    /// exactly what went on it.
    pub(crate) held: u64,
    /// What to checksum over the run, in this worker, before the block is
    /// handed back. See [`crate::checksum`].
    #[cfg(feature = "crc")]
    pub(crate) plan: ChecksumPlan,
}

/// A finished job on its way back.
pub(crate) struct Done {
    pub(crate) index: u64,
    pub(crate) out_offset: u64,
    pub(crate) unpacked_len: usize,
    /// The decoded block, or why it could not be decoded. Either way the
    /// buffer comes back so it can be used again.
    pub(crate) res: Result<(), Error>,
    pub(crate) out: Vec<u8>,
    /// The job's input buffer, returned for reuse.
    pub(crate) packed: Vec<u8>,
    /// What the dispatcher charged for the job, echoed back unchanged.
    pub(crate) held: u64,
    /// What the worker checksummed, if the run decoded and a plan asked for
    /// it.
    #[cfg(feature = "crc")]
    pub(crate) checks: Option<BlockChecks>,
}

/// Worker threads shared by every decode mode.
pub(crate) struct Pool {
    dict_prop: u8,
    job_tx: Option<Sender<Job>>,
    done_rx: Receiver<Done>,
    done_tx: Sender<Done>,
    job_rx: Arc<std::sync::Mutex<Receiver<Job>>>,
    cancel: Arc<AtomicBool>,
    /// Workers that exist and have not yet returned. Raised with the handle
    /// and lowered by the worker on its way out, so a thread the OS has not
    /// scheduled yet still counts. Only a test reads it, but it is what makes
    /// "no thread is left behind" checkable rather than asserted.
    live: Arc<AtomicUsize>,
    handles: Vec<JoinHandle<()>>,
}

impl Pool {
    pub(crate) fn new(dict_prop: u8) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<Done>();
        Pool {
            dict_prop,
            job_tx: Some(job_tx),
            done_rx,
            done_tx,
            job_rx: Arc::new(std::sync::Mutex::new(job_rx)),
            cancel: Arc::new(AtomicBool::new(false)),
            live: Arc::new(AtomicUsize::new(0)),
            handles: Vec::new(),
        }
    }

    /// How many threads have actually been spawned.
    pub(crate) fn spawned(&self) -> usize {
        self.handles.len()
    }

    /// How many worker threads are running right now.
    ///
    /// Counted from the spawn, not from the thread's first instruction: a
    /// worker the OS has created but not yet scheduled is alive, and on a
    /// loaded machine that gap is wide enough to see.
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// Spawns one more worker, if the pool is still accepting work.
    ///
    /// Workers are added on demand: a decoder configured for sixteen threads
    /// that only ever has one run in flight never creates the other fifteen.
    pub(crate) fn grow(&mut self) {
        if self.job_tx.is_none() || self.cancel.load(Ordering::Relaxed) {
            return;
        }
        let rx = Arc::clone(&self.job_rx);
        let tx = self.done_tx.clone();
        let cancel = Arc::clone(&self.cancel);
        let live = Arc::clone(&self.live);
        let prop = self.dict_prop;
        let name = alloc::format!("lzma2-mt-{}", self.handles.len());
        // Counted here rather than as the worker's first act: the count has to
        // rise with the handle it belongs to. A worker increments only once the
        // OS gets round to running it, which on a loaded machine can be long
        // after the spawn returns, and until then the pool would be reporting
        // fewer live workers than it holds handles for.
        self.live.fetch_add(1, Ordering::Relaxed);
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            worker(prop, &rx, &tx, &cancel);
            live.fetch_sub(1, Ordering::Relaxed);
        });
        // A thread that will not start is not an error: the work is simply
        // done by the threads that did, or on the caller's own stack. Its
        // closure never runs, so the count comes back down here.
        match spawned {
            Ok(h) => self.handles.push(h),
            Err(_) => {
                self.live.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Hands a run to whichever worker wakes first.
    pub(crate) fn dispatch(&self, job: Job) -> Result<(), Error> {
        match &self.job_tx {
            Some(tx) => tx.send(job).map_err(|_| Error::Cancelled),
            None => Err(Error::Cancelled),
        }
    }

    /// Takes a finished block if one is ready, without waiting.
    pub(crate) fn try_collect(&self) -> Option<Done> {
        self.done_rx.try_recv().ok()
    }

    /// Waits for the next finished block.
    ///
    /// Only ever called with work outstanding, but it still refuses to wait
    /// forever: the pool holds a sender of its own so the channel never
    /// disconnects, and a worker that has died would otherwise hang its
    /// dispatcher. If nothing arrives and no worker is alive to send it, the
    /// answer is "never".
    pub(crate) fn collect(&self) -> Option<Done> {
        loop {
            match self
                .done_rx
                .recv_timeout(core::time::Duration::from_millis(50))
            {
                Ok(d) => return Some(d),
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) => {
                    if self.handles.iter().all(JoinHandle::is_finished) {
                        return None;
                    }
                }
            }
        }
    }

    /// Stops the workers and waits for them.
    pub(crate) fn shutdown(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Dropping the sender is what wakes an idle worker: its `recv` fails.
        self.job_tx = None;
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A worker's whole life: park on the channel, decode one run, park again.
///
/// The decoder and its probability table are built once and reused; a run's
/// output buffer is its dictionary, so no worker ever allocates one.
fn worker(
    dict_prop: u8,
    rx: &std::sync::Mutex<Receiver<Job>>,
    tx: &Sender<Done>,
    cancel: &AtomicBool,
) {
    let mut dec = match Lzma2Decoder::new_probs_only(dict_prop) {
        Ok(d) => d,
        Err(_) => return,
    };

    loop {
        // The lock is held only across `recv`, which is where a worker waits.
        // `Receiver` is not `Sync`, so the queue is shared this way rather
        // than cloned; contention is one hand-off per run.
        let mut job = {
            let g = match rx.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            match g.recv() {
                Ok(j) => j,
                Err(_) => return,
            }
        };

        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(Done {
                index: job.index,
                out_offset: job.out_offset,
                unpacked_len: job.unpacked_len,
                res: Err(Error::Cancelled),
                out: job.out,
                packed: job.packed,
                held: job.held,
                #[cfg(feature = "crc")]
                checks: None,
            });
            continue;
        }

        // A worker that dies without answering would leave its dispatcher
        // waiting for a block that is never coming, so a panic is caught,
        // reported as the internal failure it is, and the decoder rebuilt.
        let res = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_run(&mut dec, &mut job)
        })) {
            Ok(r) => r,
            Err(_) => {
                dec = match Lzma2Decoder::new_probs_only(dict_prop) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = tx.send(Done {
                            index: job.index,
                            out_offset: job.out_offset,
                            unpacked_len: job.unpacked_len,
                            res: Err(e),
                            out: Vec::new(),
                            packed: job.packed,
                            held: job.held,
                            #[cfg(feature = "crc")]
                            checks: None,
                        });
                        return;
                    }
                };
                Err(Error::InternalFailure)
            }
        };
        // Checksummed here, on the worker, over the bytes it just produced -
        // never on whoever drains the output. See [`crate::checksum`].
        #[cfg(feature = "crc")]
        let checks = if res.is_ok() && !job.plan.is_none() {
            let mut seg = Segmenter::new(&job.plan, job.out_offset);
            seg.update(dec.dic_slice(0, job.unpacked_len));
            Some(seg.finish())
        } else {
            None
        };

        let out = dec.take_block_dic();
        if tx
            .send(Done {
                index: job.index,
                out_offset: job.out_offset,
                unpacked_len: job.unpacked_len,
                res,
                out,
                packed: job.packed,
                held: job.held,
                #[cfg(feature = "crc")]
                checks,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Decodes one whole run into the buffer the job carries.
///
/// C: `Lzma2DecMt_MtCallback_Code`, minus the partial-block bookkeeping: the
/// run's length is known from its headers before it is dispatched, so a
/// worker either decodes all of it or the stream is corrupt.
fn decode_run(dec: &mut Lzma2Decoder, job: &mut Job) -> Result<(), Error> {
    let want = job.unpacked_len;

    // The buffer is grown, never shrunk and never re-zeroed: `dicBufSize` says
    // how much of it this run uses, so a stream of unequal runs does not pay
    // to clear the whole thing every time.
    let mut out = core::mem::take(&mut job.out);
    if out.len() < want {
        let more = want - out.len();
        out.try_reserve_exact(more).map_err(|_| Error::Alloc)?;
        out.resize(want, 0u8);
    }
    dec.set_block_dic(out, want);

    let (used, status) = dec.decode_block(want, &job.packed, FinishMode::End)?;

    // The run's boundaries came from its own chunk headers, so a worker that
    // did not consume exactly the run, or did not produce exactly what the
    // headers promised, was given something that does not decode.
    if used != job.packed.len() || dec.dic_pos() != want {
        return Err(Error::CorruptData);
    }
    // The status itself says nothing useful here. A run ends at the next run's
    // control byte, which is not part of the job, so the decoder reports that
    // it wants more input even though the run is complete; what makes it
    // complete is the two counts above, which come from the run's own headers.
    let _ = status;
    Ok(())
}

/// Maps a decode result onto the located error the caller gets.
pub(crate) fn locate(index: u64, out_offset: u64, e: Error) -> Error {
    match e {
        Error::CorruptData => Error::CorruptRun { index, out_offset },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::Pool;

    /// A worker counts as live from the moment the pool holds its handle, not
    /// from the moment the OS gets round to running it.
    ///
    /// A spawned thread has not executed anything when `spawn` returns. While
    /// the count was the worker's own first act, the pool understated itself
    /// for as long as the thread sat unscheduled: invisible on an idle
    /// machine, and on a loaded two-core runner wide enough that eight
    /// handles reported six live workers.
    ///
    /// Read with no pause between growing and asking, which is where the old
    /// count was always wrong and the new one cannot be.
    #[test]
    fn a_worker_is_live_before_it_is_scheduled() {
        let mut pool = Pool::new(0);
        for _ in 0..8 {
            pool.grow();
            assert_eq!(pool.live(), pool.spawned(), "a handle with no live worker");
        }
        pool.shutdown();
        assert_eq!(pool.live(), 0, "a worker outlived shutdown");
    }
}
