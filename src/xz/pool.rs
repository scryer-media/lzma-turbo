//! Worker threads that decode whole xz blocks.
//!
//! C: `C/XzDecMt.c`, which schedules xz blocks across threads much as
//! `C/Lzma2DecMt.c` schedules LZMA2 runs. This is that idea on the pool shape
//! [`crate::mt::pool`] already uses - threads spawned on demand, parked on a
//! channel, reused across jobs - rather than on `MtDec.c`'s token ring, and
//! for the reasons the ring does not fit:
//!
//! - an xz block's boundaries are *known* before anything is decoded, from the
//!   index, so there is no discovery to serialise and no reason for a worker
//!   to hold a token while it looks for the next run;
//! - every block carries its own filter chain and its own dictionary
//!   property, so a worker cannot be built once for the stream;
//! - the block's uncompressed size is known too, which means the block's
//!   output buffer can *be* the dictionary (the same trick `Lzma2DecMt.c`
//!   plays with `t->dec.decoder.dic`), so a worker allocates one buffer per
//!   block and no dictionary at all.
//!
//! A worker therefore does the whole of a block: parse its header, decode the
//! LZMA2, run the converters over the finished block in place, and compute the
//! check - all of it off the thread that is draining the output, which is the
//! same rule the LZMA2 workers follow (see [`crate::checksum`]).

use alloc::sync::Arc;
use alloc::vec::Vec;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;

use super::block::{BlockHeader, header_size_from_first_byte};
use super::check::{BlockCheck, RunningCheck};
use super::error::XzErrorKind;
use super::stream::CheckType;
use crate::error::{FinishMode, Status};
use crate::lzma2::Lzma2Decoder;
use crate::mt::checksum::ChecksumPlan;

/// One whole block on its way to a worker.
pub(crate) struct XzJob {
    /// The block's position in the file's block order, counting from zero.
    pub(crate) index: u64,
    /// Which stream it belongs to, for error reporting.
    pub(crate) stream: u64,
    /// Its position within that stream, for error reporting.
    pub(crate) block_in_stream: u64,
    /// Where the block starts in the file, for error reporting.
    pub(crate) file_offset: u64,
    /// Where its output belongs in the decoded file.
    pub(crate) unpacked_offset: u64,
    /// What the index says it decodes to.
    pub(crate) unpacked_len: usize,
    /// The block's unpadded size from the index: header, data and check.
    pub(crate) unpadded_size: u64,
    /// The block as it sits in the file, padded to a multiple of four.
    pub(crate) bytes: Vec<u8>,
    /// The check every block in the stream carries.
    pub(crate) check: CheckType,
    /// Whether to compute it.
    pub(crate) verify: bool,
    /// The caller's own segment boundaries.
    pub(crate) plan: ChecksumPlan,
    /// An output buffer recycled from an earlier block.
    pub(crate) out: Vec<u8>,
}

/// A finished block on its way back.
pub(crate) struct XzDone {
    pub(crate) index: u64,
    pub(crate) stream: u64,
    pub(crate) block_in_stream: u64,
    pub(crate) file_offset: u64,
    pub(crate) unpacked_len: usize,
    /// Whether the block decoded, and why not if it did not. The buffers come
    /// back either way so they can be used again.
    pub(crate) res: Result<(), XzErrorKind>,
    pub(crate) out: Vec<u8>,
    pub(crate) bytes: Vec<u8>,
    /// What the worker checksummed, when it was asked to and the block
    /// decoded.
    pub(crate) checks: Option<BlockCheck>,
}

/// Workers that decode whole xz blocks.
pub(crate) struct XzPool {
    job_tx: Option<Sender<XzJob>>,
    done_rx: Receiver<XzDone>,
    done_tx: Sender<XzDone>,
    job_rx: Arc<Mutex<Receiver<XzJob>>>,
    cancel: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
    handles: Vec<JoinHandle<()>>,
}

impl XzPool {
    pub(crate) fn new() -> Self {
        let (job_tx, job_rx) = channel::<XzJob>();
        let (done_tx, done_rx) = channel::<XzDone>();
        XzPool {
            job_tx: Some(job_tx),
            done_rx,
            done_tx,
            job_rx: Arc::new(Mutex::new(job_rx)),
            cancel: Arc::new(AtomicBool::new(false)),
            live: Arc::new(AtomicUsize::new(0)),
            handles: Vec::new(),
        }
    }

    /// How many threads have actually been spawned.
    pub(crate) fn spawned(&self) -> usize {
        self.handles.len()
    }

    /// Spawns one more worker, on demand.
    pub(crate) fn grow(&mut self) {
        if self.job_tx.is_none() || self.cancel.load(Ordering::Relaxed) {
            return;
        }
        let rx = Arc::clone(&self.job_rx);
        let tx = self.done_tx.clone();
        let cancel = Arc::clone(&self.cancel);
        let live = Arc::clone(&self.live);
        let name = alloc::format!("xz-mt-{}", self.handles.len());
        // Counted with the handle, not from inside the worker: a thread the OS
        // has created but not yet scheduled is alive, and counting it only once
        // it runs leaves the pool understating itself on a loaded machine.
        self.live.fetch_add(1, Ordering::Relaxed);
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            worker(&rx, &tx, &cancel);
            live.fetch_sub(1, Ordering::Relaxed);
        });
        // A thread that will not start is not an error: the blocks are decoded
        // by the threads that did. Its closure never runs, so the count comes
        // back down here.
        match spawned {
            Ok(h) => self.handles.push(h),
            Err(_) => {
                self.live.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Hands a block to whichever worker wakes first.
    pub(crate) fn dispatch(&self, job: XzJob) -> Result<(), XzErrorKind> {
        match &self.job_tx {
            Some(tx) => tx
                .send(job)
                .map_err(|_| XzErrorKind::Lzma(crate::Error::Cancelled)),
            None => Err(XzErrorKind::Lzma(crate::Error::Cancelled)),
        }
    }

    /// Takes a finished block if one is already waiting, without blocking.
    /// Used by the adaptive decoder, which must stay responsive to input.
    pub(crate) fn try_collect(&self) -> Option<XzDone> {
        self.done_rx.try_recv().ok()
    }

    /// Waits for the next finished block, or `None` if no worker can send one.
    pub(crate) fn collect(&self) -> Option<XzDone> {
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
        self.job_tx = None;
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for XzPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A worker's whole life: park on the channel, decode one block, park again.
fn worker(rx: &Mutex<Receiver<XzJob>>, tx: &Sender<XzDone>, cancel: &AtomicBool) {
    loop {
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

        let (res, checks) = if cancel.load(Ordering::Relaxed) {
            (Err(XzErrorKind::Lzma(crate::Error::Cancelled)), None)
        } else {
            // A worker that died without answering would leave its dispatcher
            // waiting for a block that is never coming, so a panic is caught
            // and reported as the internal failure it is.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_block(&mut job)))
            {
                Ok(Ok(checks)) => (Ok(()), checks),
                Ok(Err(e)) => (Err(e), None),
                Err(_) => (Err(XzErrorKind::Lzma(crate::Error::InternalFailure)), None),
            }
        };

        if tx
            .send(XzDone {
                index: job.index,
                stream: job.stream,
                block_in_stream: job.block_in_stream,
                file_offset: job.file_offset,
                unpacked_len: job.unpacked_len,
                res,
                out: core::mem::take(&mut job.out),
                bytes: core::mem::take(&mut job.bytes),
                checks,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Decodes one whole block into the buffer the job carries.
///
/// C: `XzDecMt_Callback_Code` in `C/XzDecMt.c`, which is the same three steps
/// in the same order: the block header, the coder chain, the check.
fn decode_block(job: &mut XzJob) -> Result<Option<BlockCheck>, XzErrorKind> {
    let check_size = job.check.size();
    let bytes = core::mem::take(&mut job.bytes);

    let (header, header_size, packed_len) = block_header(job, &bytes)?;
    let data_end = header_size + packed_len;
    let padded_end = data_end + (4 - (packed_len % 4)) % 4;
    if padded_end + check_size > bytes.len() {
        return Err(XzErrorKind::TruncatedInput);
    }
    // Spec §3.3: validate padding before decoding, on both delivery paths.
    if bytes[data_end..padded_end].iter().any(|&b| b != 0) {
        return Err(XzErrorKind::BadPadding);
    }
    let mut dec = block_decoder(job, &header)?;
    let (used, status) = dec.decode_block(
        job.unpacked_len,
        &bytes[header_size..data_end],
        FinishMode::End,
    )?;
    let result = finish_block(
        job,
        &header,
        dec,
        used == packed_len && status == Status::FinishedWithMark,
        &bytes[padded_end..padded_end + check_size],
    );
    job.bytes = bytes;
    result
}

/// Shared index/header agreement checks for buffered workers and bounded input.
fn block_header(job: &XzJob, bytes: &[u8]) -> Result<(BlockHeader, usize, usize), XzErrorKind> {
    let check_size = job.check.size();
    // Its CRC-32 is checked before any field in it is used.
    let header_size = bytes
        .first()
        .copied()
        .and_then(header_size_from_first_byte)
        .ok_or(XzErrorKind::BadBlockHeader)?;
    if header_size as u64 + check_size as u64 > job.unpadded_size || header_size > bytes.len() {
        return Err(XzErrorKind::BadBlockHeader);
    }
    let header = BlockHeader::parse(&bytes[..header_size])?;

    // The index says how long the block is; the header may say it too, and
    // then the two have to agree.
    let packed_len = usize::try_from(job.unpadded_size - header_size as u64 - check_size as u64)
        .map_err(|_| XzErrorKind::SizeMismatch)?;
    if let Some(c) = header.compressed_size
        && c != packed_len as u64
    {
        return Err(XzErrorKind::SizeMismatch);
    }
    if let Some(u) = header.uncompressed_size
        && u != job.unpacked_len as u64
    {
        return Err(XzErrorKind::SizeMismatch);
    }
    Ok((header, header_size, packed_len))
}

fn block_decoder(job: &mut XzJob, header: &BlockHeader) -> Result<Lzma2Decoder, XzErrorKind> {
    // The LZMA2, decoded straight into the block's output buffer, which is
    // also its dictionary.
    let want = job.unpacked_len;
    let mut out = core::mem::take(&mut job.out);
    if out.len() < want {
        let more = want - out.len();
        out.try_reserve_exact(more)
            .map_err(|_| XzErrorKind::Lzma(crate::Error::Alloc))?;
        out.resize(want, 0u8);
    }
    let mut dec = Lzma2Decoder::new_probs_only(header.chain.dict_prop)?;
    dec.set_block_dic(out, want);
    Ok(dec)
}

fn finish_block(
    job: &mut XzJob,
    header: &BlockHeader,
    mut dec: Lzma2Decoder,
    exact_marked_input: bool,
    expected: &[u8],
) -> Result<Option<BlockCheck>, XzErrorKind> {
    let want = job.unpacked_len;
    let produced = dec.dic_pos();
    // Reaching the index's sizes is not the end of an LZMA2 stream: the end
    // marker is a control byte of its own, inside the compressed size, and a
    // block that runs out of compressed data without it is corrupt however
    // well its sizes line up. C: `Lzma2Dec_DecodeToDic`, which only reports
    // `LZMA_STATUS_FINISHED_WITH_MARK` from `LZMA2_STATE_FINISHED`, and the
    // `XzUnpacker_Code` caller that will not close a block before it.
    let mut out = dec.take_block_dic();
    // The block's boundaries came from the index, so a block that did not
    // consume exactly its compressed data, or did not produce exactly what
    // the index promised, does not match the file it came from.
    if produced != want || !exact_marked_input {
        job.out = out;
        return Err(XzErrorKind::SizeMismatch);
    }

    // The converters, over the whole block at once.
    if !header.chain.is_plain_lzma2() {
        let mut convs = header.chain.build()?;
        convs.apply_in_place(&mut out[..want]);
    }

    // The check, on this thread, over the bytes this thread produced.
    let mut running = RunningCheck::new(job.check, job.unpacked_offset, &job.plan, job.verify);
    running.update(&out[..want]);
    let checks = running.finish(expected);

    job.out = out;
    checks.map(Some)
}

/// C: `XzDecMt_Decode_ST` refills a bounded input buffer while retaining coder
/// state. Keep this reader's stronger contract: no output before block checks.
pub(crate) fn decode_block_reader<R: std::io::Read + std::io::Seek>(
    job: &mut XzJob,
    src: &mut R,
) -> std::io::Result<Option<BlockCheck>> {
    use std::io::SeekFrom;
    let stream = job.stream;
    let block = job.block_in_stream;
    let offset = job.file_offset;
    let located =
        |kind| std::io::Error::from(super::error::XzError::in_block(kind, stream, block, offset));
    let mut header_bytes = [0u8; 1024];
    src.read_exact(&mut header_bytes[..1])?;
    let header_size = header_size_from_first_byte(header_bytes[0])
        .ok_or_else(|| located(XzErrorKind::BadBlockHeader))?;
    if header_size as u64 + job.check.size() as u64 > job.unpadded_size {
        return Err(located(XzErrorKind::BadBlockHeader));
    }
    src.read_exact(&mut header_bytes[1..header_size])?;
    let (header, _, packed_len) =
        block_header(job, &header_bytes[..header_size]).map_err(located)?;
    let data_start = offset
        .checked_add(header_size as u64)
        .ok_or_else(|| located(XzErrorKind::SizeMismatch))?;
    let data_end = data_start
        .checked_add(packed_len as u64)
        .ok_or_else(|| located(XzErrorKind::SizeMismatch))?;
    let padding = (4 - packed_len % 4) % 4;
    let trailer_len = padding + job.check.size();
    let mut trailer = [0u8; 67];
    src.seek(SeekFrom::Start(data_end))?;
    src.read_exact(&mut trailer[..trailer_len])?;
    if trailer[..padding].iter().any(|&b| b != 0) {
        return Err(located(XzErrorKind::BadPadding));
    }
    src.seek(SeekFrom::Start(data_start))?;
    let mut dec = block_decoder(job, &header).map_err(located)?;
    let mut input = core::mem::take(&mut job.bytes);
    let capacity = packed_len.min(256 << 10);
    if input.len() < capacity {
        input
            .try_reserve_exact(capacity - input.len())
            .map_err(|_| located(XzErrorKind::Lzma(crate::Error::Alloc)))?;
        input.resize(capacity, 0);
    }
    let mut used_total = 0;
    let mut status = Status::NotSpecified;
    while used_total < packed_len {
        let size = (packed_len - used_total).min(capacity);
        src.read_exact(&mut input[..size])?;
        let (used, next_status) = dec
            .decode_block(job.unpacked_len, &input[..size], FinishMode::End)
            .map_err(|e| located(XzErrorKind::Lzma(e)))?;
        used_total += used;
        status = next_status;
        if used != size || status == Status::FinishedWithMark {
            break;
        }
    }
    job.bytes = input;
    let checks = finish_block(
        job,
        &header,
        dec,
        used_total == packed_len && status == Status::FinishedWithMark,
        &trailer[padding..trailer_len],
    )
    .map_err(located)?;
    // Match the buffered reader's cursor after the complete padded block.
    src.seek(SeekFrom::Start(data_end + trailer_len as u64))?;
    Ok(checks)
}
