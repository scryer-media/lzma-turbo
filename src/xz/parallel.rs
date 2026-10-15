//! Block-parallel decoding of a seekable `.xz` file.
//!
//! C: `C/XzDecMt.c`. The idea is the same - one thread reads blocks and hands
//! them out, workers decode whole blocks, the output is reassembled in order -
//! but the scheduling is simpler here because the index is read first: every
//! block's offset, compressed size and uncompressed size are known before a
//! byte is decoded, so there is no need for 7-Zip's "parse ahead and hope"
//! path or for its block-size guessing. The trade is that this reader needs a
//! seekable source; a pipe gets [`super::XzReader`], which is sequential.
//!
//! Memory is the other reason the index comes first. A worker costs its
//! block's output buffer (which is also its dictionary) plus the block as it
//! sits in the file, and the index says how big the largest of those is, so
//! the thread count can be *degraded to fit* the caller's limit instead of the
//! decode failing halfway through.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use std::io::{self, Read, Seek, SeekFrom};

use super::XzOptions;
use super::check::BlockCheck;
use super::error::{XzError, XzErrorKind, XzResult};
use super::index::{XzStreamIndex, stream_table};
use super::pool::{XzDone, XzJob, XzPool};
use super::stream::CheckType;

/// What a worker costs besides its buffers: the LZMA2 probability table, which
/// is `1846 + (0x300 << (lc + lp))` sixteen-bit entries at the maximum
/// `lc + lp` LZMA2 allows, rounded up.
const PROBS_BYTES: u64 = 32 << 10;

/// One block of the file, as the index describes it.
#[derive(Debug, Clone, Copy)]
struct Plan {
    stream: u64,
    block_in_stream: u64,
    file_offset: u64,
    unpacked_offset: u64,
    unpacked_len: usize,
    unpadded_size: u64,
    padded_size: u64,
    check: CheckType,
}

/// A `Read` adapter that decodes an `.xz` file's blocks in parallel.
///
/// Construction reads the index of every stream in the file, which is where
/// all the structural validation happens: a file this cannot map is refused
/// here, before any thread is started, and the caller falls back to
/// [`super::XzReader`]. Decoding then reads blocks in file order, hands them
/// to workers, and reassembles the output in order.
///
/// The source has to be `Read + Seek`, but not `Send`: it is only ever read on
/// the caller's own thread.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::Read;
/// use lzma_turbo::xz::{XzOptions, XzParallelReader, XzReader};
///
/// # fn main() -> std::io::Result<()> {
/// let file = File::open("big.tar.xz")?;
/// let opts = XzOptions::default().with_threads(8).with_memory_limit(1 << 30);
/// let mut out = Vec::new();
/// match XzParallelReader::with_options(file, opts) {
///     Ok(mut r) => r.read_to_end(&mut out)?,
///     // Not a shape that can be decoded in parallel - a pipe-written file
///     // with no usable index, say. The sequential reader takes anything.
///     Err(_) => XzReader::new(File::open("big.tar.xz")?).read_to_end(&mut out)?,
/// };
/// # Ok(())
/// # }
/// ```
pub struct XzParallelReader<R: Read + Seek> {
    src: R,
    opts: XzOptions,
    blocks: Vec<Plan>,
    /// Where `src` is positioned, so a contiguous run of blocks is read
    /// without a seek between them.
    pos: u64,
    next_dispatch: usize,
    next_emit: u64,
    pool: XzPool,
    threads: usize,
    in_flight: usize,
    ready: BTreeMap<u64, XzDone>,
    /// The block being handed to the caller.
    current: Option<(Vec<u8>, usize, usize)>,
    spare_out: Vec<Vec<u8>>,
    spare_in: Vec<Vec<u8>>,
    checks: Vec<BlockCheck>,
    total_out: u64,
    per_worker: u64,
    failed: bool,
}

impl<R: Read + Seek> XzParallelReader<R> {
    /// Maps the file and prepares to decode it with the default options.
    ///
    /// # Errors
    ///
    /// Any structural error in any stream's footer, index or header, and
    /// [`XzErrorKind::MemoryLimit`] if even one worker would not fit.
    pub fn new(src: R) -> XzResult<Self> {
        Self::with_options(src, XzOptions::default())
    }

    /// As [`XzParallelReader::new`], with options.
    ///
    /// # Errors
    ///
    /// As [`XzParallelReader::new`], plus [`XzErrorKind::TooMuchOutput`] if
    /// the index says the file decodes to more than the caller allows - which
    /// is known here, before anything is decoded.
    pub fn with_options(mut src: R, opts: XzOptions) -> XzResult<Self> {
        let streams = stream_table(&mut src, opts.memory_limit)?;
        let blocks = plan_blocks(&streams, &opts)?;

        let largest_out = blocks
            .iter()
            .map(|b| b.unpacked_len as u64)
            .max()
            .unwrap_or(0);
        let largest_in = blocks.iter().map(|b| b.padded_size).max().unwrap_or(0);
        let per_worker = largest_out
            .checked_add(largest_in)
            .and_then(|v| v.checked_add(PROBS_BYTES))
            .ok_or_else(|| XzError::at(XzErrorKind::SizeMismatch, 0, 0))?;
        if per_worker > opts.memory_limit {
            return Err(XzError::at(
                XzErrorKind::MemoryLimit {
                    needed: per_worker,
                    limit: opts.memory_limit,
                },
                0,
                0,
            ));
        }

        let want = if opts.threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            opts.threads
        };
        // Degrade rather than fail: the decode runs with as many workers as
        // the limit pays for, down to one.
        let affordable =
            usize::try_from(opts.memory_limit / per_worker.max(1)).unwrap_or(usize::MAX);
        let threads = want.min(affordable).min(blocks.len()).max(1);

        Ok(XzParallelReader {
            src,
            opts,
            blocks,
            pos: u64::MAX,
            next_dispatch: 0,
            next_emit: 0,
            pool: XzPool::new(),
            threads,
            in_flight: 0,
            ready: BTreeMap::new(),
            current: None,
            spare_out: Vec::new(),
            spare_in: Vec::new(),
            checks: Vec::new(),
            total_out: 0,
            per_worker,
            failed: false,
        })
    }

    /// How many worker threads this decode will use, after degrading to fit
    /// the memory limit.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// What the decode will cost at its peak, in bytes: one block's output
    /// buffer, one block's compressed bytes and one probability table per
    /// worker, sized by the largest block in the file.
    #[must_use]
    pub fn memory_estimate(&self) -> u64 {
        self.per_worker.saturating_mul(self.threads as u64)
    }

    /// How many blocks the file has, across every stream.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// What the index says the file decodes to.
    #[must_use]
    pub fn uncompressed_size(&self) -> u64 {
        self.blocks.iter().map(|b| b.unpacked_len as u64).sum()
    }

    /// The per-block checks computed so far, in block order. Only collected
    /// when the options carry a [`crate::ChecksumPlan`].
    #[must_use]
    pub fn block_checks(&self) -> &[BlockCheck] {
        &self.checks
    }

    /// Gives the underlying source back.
    pub fn into_inner(self) -> R {
        self.src
    }

    /// Reads one block's bytes and hands them to a worker.
    fn dispatch_one(&mut self) -> io::Result<()> {
        let plan = self.blocks[self.next_dispatch];
        if self.pos != plan.file_offset {
            self.src.seek(SeekFrom::Start(plan.file_offset))?;
            self.pos = plan.file_offset;
        }
        let want = usize::try_from(plan.padded_size).map_err(|_| {
            io::Error::from(XzError::at(
                XzErrorKind::SizeMismatch,
                plan.stream,
                plan.file_offset,
            ))
        })?;
        let mut bytes = self.spare_in.pop().unwrap_or_default();
        bytes.clear();
        // The index gives the exact bound. Geometric growth here can retain
        // a second packed block when the next block is only a few bytes larger.
        bytes.try_reserve_exact(want).map_err(|_| {
            io::Error::from(XzError::at(
                XzErrorKind::MemoryLimit {
                    needed: plan.padded_size,
                    limit: self.opts.memory_limit,
                },
                plan.stream,
                plan.file_offset,
            ))
        })?;
        bytes.resize(want, 0u8);
        self.src.read_exact(&mut bytes)?;
        self.pos += plan.padded_size;

        // Grow the pool only as far as there is work for it.
        if self.pool.spawned() < self.threads {
            self.pool.grow();
        }
        let job = XzJob {
            index: self.next_dispatch as u64,
            stream: plan.stream,
            block_in_stream: plan.block_in_stream,
            file_offset: plan.file_offset,
            unpacked_offset: plan.unpacked_offset,
            unpacked_len: plan.unpacked_len,
            unpadded_size: plan.unpadded_size,
            bytes,
            check: plan.check,
            verify: self.opts.verify_checks,
            plan: self.opts.plan.clone(),
            out: self.spare_out.pop().unwrap_or_default(),
        };
        self.pool
            .dispatch(job)
            .map_err(|k| io::Error::from(XzError::at(k, plan.stream, plan.file_offset)))?;
        self.next_dispatch += 1;
        self.in_flight += 1;
        Ok(())
    }

    /// Makes sure the next block in output order is in hand.
    fn advance(&mut self) -> io::Result<bool> {
        if self.threads == 1 && self.next_dispatch < self.blocks.len() {
            // C: XzDecMt_Decode_ST uses bounded input on the caller thread.
            // Preserve indexed, whole-block verification before adoption.
            let plan = self.blocks[self.next_dispatch];
            if self.pos != plan.file_offset {
                self.src.seek(SeekFrom::Start(plan.file_offset))?;
            }
            self.pos = u64::MAX;
            let mut job = XzJob {
                index: self.next_dispatch as u64,
                stream: plan.stream,
                block_in_stream: plan.block_in_stream,
                file_offset: plan.file_offset,
                unpacked_offset: plan.unpacked_offset,
                unpacked_len: plan.unpacked_len,
                unpadded_size: plan.unpadded_size,
                bytes: self.spare_in.pop().unwrap_or_default(),
                check: plan.check,
                verify: self.opts.verify_checks,
                plan: self.opts.plan.clone(),
                out: self.spare_out.pop().unwrap_or_default(),
            };
            let checks = match super::pool::decode_block_reader(&mut job, &mut self.src) {
                Ok(checks) => checks,
                Err(error) => {
                    // As in the worker path, malformed blocks stop delivery.
                    // A source I/O error remains retryable with a fresh seek.
                    self.failed = error.get_ref().is_some_and(|e| e.is::<XzError>());
                    return Err(error);
                }
            };
            self.pos = plan.file_offset + plan.padded_size;
            self.next_dispatch += 1;
            self.adopt(XzDone {
                index: job.index,
                stream: job.stream,
                block_in_stream: job.block_in_stream,
                file_offset: job.file_offset,
                unpacked_len: job.unpacked_len,
                res: Ok(()),
                out: job.out,
                bytes: job.bytes,
                checks,
            })?;
            return Ok(true);
        }
        loop {
            if let Some(done) = self.ready.remove(&self.next_emit) {
                return self.adopt(done).map(|()| true);
            }
            // Completed blocks waiting for order still own both buffers.
            // Release their admission slot only when adopted, not collected;
            // otherwise a slow earlier block permits an archive-sized backlog.
            while self.in_flight + self.ready.len() < self.threads
                && self.next_dispatch < self.blocks.len()
            {
                self.dispatch_one()?;
            }
            if self.in_flight == 0 {
                // Nothing outstanding and nothing left to send: the file is
                // decoded.
                return Ok(false);
            }
            let Some(done) = self.pool.collect() else {
                return Err(io::Error::from(XzError::at(
                    XzErrorKind::Lzma(crate::Error::InternalFailure),
                    0,
                    0,
                )));
            };
            self.in_flight -= 1;
            self.ready.insert(done.index, done);
        }
    }

    /// Takes a finished block as the current output, or reports why it failed.
    fn adopt(&mut self, done: XzDone) -> io::Result<()> {
        let XzDone {
            index: _,
            stream,
            block_in_stream,
            file_offset,
            unpacked_len,
            res,
            out,
            bytes,
            checks,
        } = done;
        self.spare_in.push(bytes);
        if let Err(kind) = res {
            self.failed = true;
            self.spare_out.push(out);
            return Err(io::Error::from(XzError::in_block(
                kind,
                stream,
                block_in_stream,
                file_offset,
            )));
        }
        if let Some(c) = checks
            && !self.opts.plan.is_none()
        {
            self.checks.push(c);
        }
        self.next_emit += 1;
        self.current = Some((out, 0, unpacked_len));
        Ok(())
    }
}

impl<R: Read + Seek> Read for XzParallelReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.failed {
            return Ok(0);
        }
        loop {
            if let Some((buf, pos, len)) = self.current.as_mut() {
                if *pos < *len {
                    let n = (*len - *pos).min(out.len());
                    out[..n].copy_from_slice(&buf[*pos..*pos + n]);
                    *pos += n;
                    self.total_out += n as u64;
                    return Ok(n);
                }
                let (buf, _, _) = self.current.take().expect("current");
                self.spare_out.push(buf);
            }
            if !self.advance()? {
                return Ok(0);
            }
        }
    }
}

/// Turns the indexes into the block list, checking the caller's caps against
/// what the index says before anything is decoded.
fn plan_blocks(streams: &[XzStreamIndex], opts: &XzOptions) -> XzResult<Vec<Plan>> {
    let mut blocks = Vec::new();
    let mut unpacked_offset = 0u64;
    let mut total = 0u64;
    for (stream_no, s) in streams.iter().enumerate() {
        let stream = stream_no as u64;
        let entries = s
            .index
            .blocks(s.stream_offset)
            .ok_or_else(|| XzError::at(XzErrorKind::IndexMismatch, stream, s.stream_offset))?;
        for (i, e) in entries.iter().enumerate() {
            let unpacked_len = usize::try_from(e.record.uncompressed_size).map_err(|_| {
                XzError::at(
                    XzErrorKind::TooMuchOutput { limit: 0 },
                    stream,
                    e.file_offset,
                )
            })?;
            if e.record.uncompressed_size > opts.max_block_size {
                return Err(XzError::in_block(
                    XzErrorKind::TooMuchOutput {
                        limit: opts.max_block_size,
                    },
                    stream,
                    i as u64,
                    e.file_offset,
                ));
            }
            total = total
                .checked_add(e.record.uncompressed_size)
                .ok_or_else(|| XzError::at(XzErrorKind::SizeMismatch, stream, e.file_offset))?;
            if let Some(cap) = opts.max_unpack_bytes
                && total > cap
            {
                return Err(XzError::in_block(
                    XzErrorKind::TooMuchOutput { limit: cap },
                    stream,
                    i as u64,
                    e.file_offset,
                ));
            }
            let padded_size = e
                .record
                .padded_size()
                .ok_or_else(|| XzError::at(XzErrorKind::IndexMismatch, stream, e.file_offset))?;
            blocks.push(Plan {
                stream,
                block_in_stream: i as u64,
                file_offset: e.file_offset,
                unpacked_offset,
                unpacked_len,
                unpadded_size: e.record.unpadded_size,
                padded_size,
                check: s.header.flags.check,
            });
            unpacked_offset += e.record.uncompressed_size;
        }
        // A stream whose check this build cannot compute is refused here for
        // the same reason the sequential reader refuses it: unchecked bytes
        // are never returned by accident.
        let check = s.header.flags.check;
        if opts.verify_checks && !check.is_verifiable() && !opts.allow_unverifiable {
            return Err(XzError::at(
                XzErrorKind::UnsupportedCheck,
                stream,
                s.stream_offset,
            ));
        }
    }
    // A stream with no blocks at all is well formed - `xz` writes one for an
    // empty input - so an empty plan is a file that decodes to nothing, not a
    // file this reader cannot map. C: `XzDecMt_Decode`, whose block loop runs
    // zero times and still returns `SZ_OK`.
    Ok(blocks)
}

#[cfg(all(test, feature = "enc"))]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bounded_single_worker_input_preserves_verified_delivery() {
        struct ShortReads {
            source: Cursor<Vec<u8>>,
            calls: usize,
            largest: usize,
        }
        impl Read for ShortReads {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                self.largest = self.largest.max(bytes.len());
                if self.calls.is_multiple_of(7) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let n = bytes.len().min(997);
                self.source.read(&mut bytes[..n])
            }
        }
        impl Seek for ShortReads {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.source.seek(position)
            }
        }
        let mut state = 0x12345678u32;
        let plain: Vec<u8> = (0..1_100_003)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let props = crate::LzmaEncProps::new()
            .with_level(1)
            .with_dict_size(4096);
        let archive = crate::encode_xz(&plain, &props, CheckType::Crc64, 400_001).unwrap();
        let source = ShortReads {
            source: Cursor::new(archive.clone()),
            calls: 0,
            largest: 0,
        };
        let mut reader =
            XzParallelReader::with_options(source, XzOptions::default().with_threads(1)).unwrap();
        let first = reader.blocks[0];
        reader.src.largest = 0;
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        assert_eq!(output, plain);
        assert!(reader.src.largest <= 256 << 10);
        assert!(reader.spare_in.iter().all(|b| b.capacity() <= 256 << 10));
        assert_eq!(reader.pool.spawned(), 0);
        // A slightly larger packed block must not double reusable storage.
        let mut queued = XzParallelReader::with_options(
            Cursor::new(&archive),
            XzOptions::default().with_threads(2),
        )
        .unwrap();
        let packed = first.padded_size as usize;
        queued.spare_in.push(Vec::with_capacity(packed - 1));
        queued.dispatch_one().unwrap();
        let done = queued.pool.collect().unwrap();
        done.res.unwrap();
        assert_eq!(done.bytes.capacity(), packed);
        let start = first.file_offset as usize;
        let header_size = super::super::block::header_size_from_first_byte(archive[start]).unwrap();
        // Both an early end marker and a corrupt check must expose no bytes
        // of the failed block, exactly as the buffered worker does.
        for at in [start + header_size, start + first.padded_size as usize - 1] {
            let mut bad = archive.clone();
            if at == start + header_size {
                bad[at] = 0;
            } else {
                bad[at] ^= 0x80;
            }
            let mut errors = Vec::new();
            for threads in [1, 2] {
                let mut r = XzParallelReader::with_options(
                    Cursor::new(&bad),
                    XzOptions::default().with_threads(threads),
                )
                .unwrap();
                let mut output = Vec::new();
                let error = r.read_to_end(&mut output).unwrap_err();
                assert!(output.is_empty());
                errors.push(
                    error
                        .get_ref()
                        .unwrap()
                        .downcast_ref::<XzError>()
                        .unwrap()
                        .kind,
                );
                assert_eq!(r.read(&mut [0; 1]).unwrap(), 0);
            }
            assert_eq!(errors[0], errors[1]);
        }
    }

    #[test]
    fn completed_out_of_order_blocks_keep_their_memory_admission_slots() {
        struct ReadProbe {
            source: Cursor<Vec<u8>>,
            armed: bool,
            reads: usize,
        }
        impl Read for ReadProbe {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.armed {
                    self.reads += 1;
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "unexpected read-ahead",
                    ));
                }
                self.source.read(bytes)
            }
        }
        impl Seek for ReadProbe {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.source.seek(position)
            }
        }
        let props = crate::LzmaEncProps::new()
            .with_level(1)
            .with_dict_size(4096);
        let archive = crate::encode_xz(&[7; 256], &props, CheckType::Crc64, 64).unwrap();
        let source = ReadProbe {
            source: Cursor::new(archive),
            armed: false,
            reads: 0,
        };
        let mut reader =
            XzParallelReader::with_options(source, XzOptions::default().with_threads(3)).unwrap();
        assert_eq!(reader.blocks.len(), 4);
        // Reproduce the scheduler state deterministically: block zero remains
        // in flight while one and two have completed. All three buffer slots
        // are occupied, so block three must not even read its packed input.
        reader.in_flight = 1;
        reader.next_dispatch = 3;
        for index in [1, 2] {
            let plan = reader.blocks[index];
            reader.ready.insert(
                index as u64,
                XzDone {
                    index: index as u64,
                    stream: plan.stream,
                    block_in_stream: plan.block_in_stream,
                    file_offset: plan.file_offset,
                    unpacked_len: plan.unpacked_len,
                    res: Ok(()),
                    out: Vec::new(),
                    bytes: Vec::new(),
                    checks: None,
                },
            );
        }
        reader.src.armed = true;
        // This scheduler fixture intentionally has no real worker. After
        // refusing read-ahead it reports that no worker can supply block zero.
        let error = reader.advance().unwrap_err();
        assert_eq!(
            reader.src.reads, 0,
            "completed output lost its admission slot"
        );
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(reader.next_dispatch, 3);
        assert_eq!(reader.ready.len(), 2);
    }
}
