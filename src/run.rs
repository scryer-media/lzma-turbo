//! Bounded dictionary decoding of one independently reset LZMA2 run.
//!
//! Output is provisional until [`Lzma2RunDecoder::finish`] succeeds. A sink
//! writing into shared files must bind writes to its current repair epoch,
//! discard failed runs, and publish checksum records only after completion.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "crc")]
use crate::checksum::{BlockChecks, ChecksumPlan, Segmenter};
use crate::{Error, FinishMode, Lzma2Decoder, Lzma2Run, Progress, Status};

const STEP: usize = 256 << 10;
const STATE_BYTES: u64 = 64 << 10;

/// The successful completion of one run. No record is returned on failure.
pub struct RunCompletion {
    /// The run's index supplied by the caller.
    pub index: u64,
    /// Its absolute output offset.
    pub out_offset: u64,
    /// Its exact decoded length.
    pub unpacked_len: u64,
    /// Worker-computed checksums, cut at the configured file boundaries.
    #[cfg(feature = "crc")]
    pub checks: BlockChecks,
}

/// An incremental run decoder with a capped ring dictionary and no output queue.
///
/// The packed range is exactly [`Lzma2Run::packed_len`]: it excludes the next
/// run's control byte and the stream's EOS byte, as reported by the scanner.
/// The caller supplies that range in order. The decoder never reads a source,
/// spawns a thread, or retains an input slice after a call returns.
///
/// The write-at callback may accept a prefix, return `Interrupted`, or fail.
/// A zero-length write is an error. Cancellation is checked between bounded
/// decode steps and partial writes. A blocking callback must implement its own
/// cancellation/wakeup policy; this decoder cannot interrupt arbitrary I/O.
/// Any error permanently poisons the instance. Earlier writes remain visible
/// to the sink and must be treated as unverified until `finish` succeeds.
pub struct Lzma2RunDecoder {
    decoder: Lzma2Decoder,
    run: Lzma2Run,
    index: u64,
    packed: u64,
    unpacked: u64,
    poisoned: bool,
    #[cfg(feature = "crc")]
    memory_room: u64,
    #[cfg(feature = "crc")]
    checks: Segmenter,
}

impl Lzma2RunDecoder {
    /// Conservative dictionary and coder-state reservation before allocation.
    /// Input ownership and sink buffering must be charged separately.
    ///
    /// # Errors
    /// Returns `UnsupportedProps` for an invalid dictionary property.
    pub fn memory_required(dict_prop: u8, unpacked_len: u64) -> Result<u64, Error> {
        if dict_prop > 40 {
            return Err(Error::UnsupportedProps);
        }
        let declared = crate::lzma2::frame::dic_size_from_prop_full(dict_prop) as u64;
        let dictionary = declared.min(unpacked_len).max(4096) as u32;
        let props = crate::LzmaProps::new(4, 0, 0, dictionary)?;
        Ok(props.dic_buf_size() as u64 + STATE_BYTES)
    }

    /// Additional segment storage for a checksum plan over this exact run.
    /// The plan's shared split-point allocation belongs to the caller.
    ///
    /// # Errors
    /// Invalid or overflowing output bounds return `Param`.
    #[cfg(feature = "crc")]
    pub fn checksum_memory_required(
        plan: &ChecksumPlan,
        offset: u64,
        len: u64,
    ) -> Result<u64, Error> {
        let end = offset.checked_add(len).ok_or(Error::Param)?;
        if len == 0
            || !matches!(
                plan.kind(),
                crate::Checksum::Crc32 | crate::Checksum::Crc64Xz
            )
        {
            return Ok(0);
        }
        let splits = plan.split_points();
        let first = splits.partition_point(|&point| point <= offset);
        let last = splits.partition_point(|&point| point < end);
        (last - first + 1)
            .checked_mul(core::mem::size_of::<crate::Segment>())
            .map(|bytes| bytes as u64)
            .ok_or(Error::Alloc)
    }

    /// Allocates one ring after enforcing a caller-supplied memory bound.
    ///
    /// # Errors
    /// Invalid run bounds return a located `CorruptRun`; invalid properties
    /// return `UnsupportedProps`, and an insufficient budget or allocation
    /// failure returns `Alloc`. The descriptor's lengths are still verified
    /// against the actual decoded stream before completion.
    pub fn new(index: u64, dict_prop: u8, run: Lzma2Run, memory_limit: u64) -> Result<Self, Error> {
        if !run.has_dict_reset
            || run.packed_len == 0
            || run.unpacked_len == 0
            || run.in_offset.checked_add(run.packed_len).is_none()
            || run.out_offset.checked_add(run.unpacked_len).is_none()
        {
            return Err(Error::CorruptRun {
                index,
                out_offset: run.out_offset,
            });
        }
        if Self::memory_required(dict_prop, run.unpacked_len)? > memory_limit {
            return Err(Error::Alloc);
        }
        let decoder =
            Lzma2Decoder::new_capped(dict_prop, run.unpacked_len.min(u32::MAX as u64) as u32)?;
        Ok(Self {
            decoder,
            run,
            index,
            packed: 0,
            unpacked: 0,
            poisoned: false,
            #[cfg(feature = "crc")]
            memory_room: memory_limit - Self::memory_required(dict_prop, run.unpacked_len)?,
            #[cfg(feature = "crc")]
            checks: Segmenter::new(&ChecksumPlan::none(), run.out_offset),
        })
    }

    /// Configures checksums before decoding any input.
    ///
    /// # Errors
    /// Returns `Param` after input or output has been consumed or an error,
    /// and `Alloc` when segment records exceed the constructor's memory bound.
    #[cfg(feature = "crc")]
    pub fn set_checksum_plan(&mut self, plan: &ChecksumPlan) -> Result<(), Error> {
        if self.packed != 0 || self.unpacked != 0 || self.poisoned {
            self.poisoned = true;
            return Err(Error::Param);
        }
        let bytes =
            Self::checksum_memory_required(plan, self.run.out_offset, self.run.unpacked_len)?;
        if bytes > self.memory_room {
            self.poisoned = true;
            return Err(Error::Alloc);
        }
        // Drop a previous empty plan before allocating its replacement so
        // repeated configuration cannot transiently double the reservation.
        self.checks = Segmenter::new(&ChecksumPlan::none(), self.run.out_offset);
        let mut checks = Segmenter::new(plan, self.run.out_offset);
        if let Err(error) =
            checks.reserve_segments(bytes as usize / core::mem::size_of::<crate::Segment>())
        {
            self.poisoned = true;
            return Err(error);
        }
        self.checks = checks;
        Ok(())
    }

    fn corrupt(&self) -> io::Error {
        Error::corrupt_run(self.index, self.run.out_offset).into()
    }

    /// Consumes a prefix of `input`, delivering at most `max_output` bytes.
    /// Ring slices are borrowed only for the callback; copying is the sink's
    /// decision. A partial callback write is retried at the advanced offset.
    ///
    /// `max_output == 0` makes no progress. Resubmit the unconsumed input from
    /// `Progress::read` on the next call. Input larger than the remaining packed
    /// range, including an EOS byte, is rejected before any output is written.
    ///
    /// A call that consumes all of `input` and returns `written < max_output`
    /// has delivered every byte that input decodes to. Once the run's last
    /// packed byte has been consumed by such a call, [`finish`](Self::finish)
    /// may follow directly: no call with empty input is needed to drain it.
    /// A call that stops at `max_output` may still owe output; call again
    /// with the remaining input, or with an empty slice if none remains.
    ///
    /// # Errors
    /// Corrupt data errors retain the run index and output offset. Sink errors pass
    /// through unchanged; cancellation is `Error::Cancelled` in an
    /// `Interrupted` I/O error. All errors poison the decoder.
    pub fn decode<F>(
        &mut self,
        input: &[u8],
        max_output: usize,
        cancel: &AtomicBool,
        sink: &mut F,
    ) -> io::Result<Progress>
    where
        F: FnMut(u64, &[u8]) -> io::Result<usize> + ?Sized,
    {
        let result = self.decode_inner(input, max_output, cancel, sink);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn decode_inner<F>(
        &mut self,
        input: &[u8],
        max_output: usize,
        cancel: &AtomicBool,
        sink: &mut F,
    ) -> io::Result<Progress>
    where
        F: FnMut(u64, &[u8]) -> io::Result<usize> + ?Sized,
    {
        if self.poisoned || input.len() as u64 > self.run.packed_len - self.packed {
            return Err(self.corrupt());
        }
        let mut read = 0;
        let mut written = 0;
        let mut status = Status::NotFinished;
        while written < max_output {
            check_cancel(cancel)?;
            self.decoder.wrap_dic_pos();
            let start = self.decoder.dic_pos();
            let left = usize::try_from(self.run.unpacked_len - self.unpacked).unwrap_or(usize::MAX);
            let stored = self.decoder.stored_payload(&input[read..]);
            let step = STEP
                .min(max_output - written)
                .min(left)
                .min(self.decoder.dic_buf_size() - start)
                .min(stored.map_or(usize::MAX, |(_, len)| len));
            let mode = if step == left {
                FinishMode::End
            } else {
                FinishMode::Any
            };
            let (used, next_status) = self
                .decoder
                .decode_block(start + step, &input[read..], mode)
                .map_err(|error| match error {
                    Error::CorruptData => self.corrupt(),
                    other => other.into(),
                })?;
            if next_status == Status::FinishedWithMark {
                return Err(self.corrupt());
            }
            let end = self.decoder.dic_pos();
            let produced = end - start;
            let bytes = match stored {
                Some((prefix, _)) if produced != 0 && used == prefix + produced => {
                    &input[read + prefix..read + prefix + produced]
                }
                _ => self.decoder.dic_slice(start, end),
            };
            let mut sent = 0;
            while sent < bytes.len() {
                check_cancel(cancel)?;
                match sink(
                    self.run.out_offset + self.unpacked + sent as u64,
                    &bytes[sent..],
                ) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(size) if size <= bytes.len() - sent => sent += size,
                    Ok(_) => {
                        // The sink claimed bytes it was not given.
                        return Err(Error::Param.into());
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            #[cfg(feature = "crc")]
            self.checks.update(bytes);
            self.packed += used as u64;
            self.unpacked += produced as u64;
            read += used;
            written += produced;
            status = next_status;
            // Stop when the decoder is stuck, or when the input is spent and
            // the step came back short: a short step means the coder needs
            // bytes it does not have. A full step after the last input byte
            // may still owe output (a match running past the step or the ring
            // end, or range-coder state already holding the next symbols), so
            // keep going until `max_output` or a short step; the caller then
            // never has to drain with empty input.
            if (used == 0 && produced == 0) || (read == input.len() && produced < step) {
                break;
            }
        }
        Ok(Progress {
            read,
            written,
            status,
        })
    }

    /// Verifies exact lengths and a complete chunk boundary, then checks a
    /// synthetic EOS. The source's real EOS belongs to the container scanner.
    /// Successful return verifies decoding, not an expected container CRC;
    /// the caller compares/folds the returned checks before publishing files.
    ///
    /// # Errors
    /// A poisoned, truncated, oversized, or incomplete run returns its located
    /// `CorruptRun`; cancellation returns `Cancelled`. No completion escapes.
    pub fn finish(mut self, cancel: &AtomicBool) -> Result<RunCompletion, Error> {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let corrupt = Error::CorruptRun {
            index: self.index,
            out_offset: self.run.out_offset,
        };
        if self.poisoned
            || self.packed != self.run.packed_len
            || self.unpacked != self.run.unpacked_len
            || !self.decoder.at_chunk_boundary()
        {
            return Err(corrupt);
        }
        let (used, status) = self
            .decoder
            .decode_block(self.decoder.dic_pos(), &[0], FinishMode::End)
            .map_err(|_| corrupt)?;
        if used != 1 || status != Status::FinishedWithMark {
            return Err(corrupt);
        }
        Ok(RunCompletion {
            index: self.index,
            out_offset: self.run.out_offset,
            unpacked_len: self.unpacked,
            #[cfg(feature = "crc")]
            checks: self.checks.finish(),
        })
    }
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled.into())
    } else {
        Ok(())
    }
}
