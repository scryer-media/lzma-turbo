//! Decoding one block: LZMA2, then the chain's converters, then the check.
//!
//! C: `XzDec_DecodeBlock` and the `CXzUnpacker` filter loop in `XzDec.c`,
//! collapsed into one object because this crate has exactly one last-filter.
//!
//! Two paths, and the fast one is the point: a block whose chain is a bare
//! LZMA2 filter decodes straight into the caller's buffer, with the check
//! computed over that buffer and nothing copied. Filter chains also convert
//! in caller output; only short destinations need a bounded tail buffer.

use alloc::vec::Vec;

use super::check::{BlockCheck, RunningCheck};
use super::error::XzErrorKind;
use super::filter::{Converters, FilterChain};
use super::stream::CheckType;
use crate::error::{FinishMode, Status};
use crate::lzma2::Lzma2Decoder;
use crate::mt::checksum::ChecksumPlan;

/// The maximum LZMA2 output produced per turn through a filter chain.
const SCRATCH: usize = 1 << 16;

/// What one block decode is allowed to cost and to produce.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockLimits {
    /// Bytes the decoder may allocate for this block.
    pub(crate) memory_limit: u64,
    /// The uncompressed size the header declares, if any. Used both to check
    /// the block and to shrink the dictionary.
    pub(crate) uncompressed_size: Option<u64>,
    /// The compressed size the header declares, if any.
    pub(crate) compressed_size: Option<u64>,
    /// A cap on the block's output whether or not the header declares one.
    pub(crate) max_block_size: u64,
}

/// One block, decoding.
pub(crate) struct BlockDecoder {
    lzma2: Lzma2Decoder,
    converters: Converters,
    check: RunningCheck,
    limits: BlockLimits,
    /// Output produced by the converters and not yet handed to the caller.
    pending: Vec<u8>,
    pending_len: usize,
    pending_pos: usize,
    unpacked: u64,
    packed: u64,
    /// True once LZMA2 has reached its end marker and the converters have
    /// been flushed.
    finished: bool,
}

/// The dictionary a block needs, in bytes, after clamping to what it can
/// possibly use.
///
/// The dictionary never has to be bigger than the block's own output: xz
/// resets the dictionary at every block, so no match can reach further back
/// than the block's first byte. When the header declares an uncompressed size
/// this turns a 64 MiB `xz -9` dictionary into a few kilobytes for a small
/// block, which is the difference between fitting a caller's limit and not.
pub(crate) fn dict_bytes(dict_prop: u8, uncompressed_size: Option<u64>) -> u64 {
    let declared = u64::from(crate::lzma2::frame::dic_size_from_prop_full(dict_prop));
    match uncompressed_size {
        Some(n) => declared.min(n.max(u64::from(crate::lzma::consts::LZMA_DIC_MIN))),
        None => declared,
    }
}

impl BlockDecoder {
    /// Builds a decoder for one block.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::MemoryLimit`] if the dictionary does not fit,
    /// [`XzErrorKind::TooMuchOutput`] if the header declares more output than
    /// `max_block_size`, and the LZMA2 errors for an impossible property byte.
    pub(crate) fn new(
        chain: &FilterChain,
        check: CheckType,
        unpacked_offset: u64,
        plan: &ChecksumPlan,
        verify: bool,
        limits: BlockLimits,
    ) -> Result<Self, XzErrorKind> {
        if let Some(n) = limits.uncompressed_size
            && n > limits.max_block_size
        {
            return Err(XzErrorKind::TooMuchOutput {
                limit: limits.max_block_size,
            });
        }
        let dict = dict_bytes(chain.dict_prop, limits.uncompressed_size);
        if dict > limits.memory_limit {
            return Err(XzErrorKind::MemoryLimit {
                needed: dict,
                limit: limits.memory_limit,
            });
        }
        let cap = u32::try_from(dict).unwrap_or(u32::MAX);
        let lzma2 = Lzma2Decoder::new_capped(chain.dict_prop, cap)?;
        Ok(BlockDecoder {
            lzma2,
            converters: chain.build()?,
            check: RunningCheck::new(check, unpacked_offset, plan, verify),
            limits,
            pending: Vec::new(),
            pending_len: 0,
            pending_pos: 0,
            unpacked: 0,
            packed: 0,
            finished: false,
        })
    }

    /// Bytes of compressed data consumed so far.
    pub(crate) fn packed(&self) -> u64 {
        self.packed
    }

    /// Bytes of output produced so far.
    pub(crate) fn unpacked(&self) -> u64 {
        self.unpacked
    }

    /// True once the block's end marker has been read and everything the
    /// converters held has been handed over.
    pub(crate) fn finished(&self) -> bool {
        self.finished && self.pending_pos == self.pending_len
    }

    /// Decodes into `out`, taking what it needs from `input`.
    ///
    /// Returns `(input consumed, output written)`. A return of `(0, 0)` with
    /// `finished()` false means more input is needed.
    ///
    /// # Errors
    ///
    /// The LZMA2 errors, [`XzErrorKind::TooMuchOutput`] if the block runs past
    /// its declared or configured size, and [`XzErrorKind::SizeMismatch`] if it
    /// ends somewhere else than where the header said it would.
    pub(crate) fn decode(
        &mut self,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<(usize, usize), XzErrorKind> {
        // Whatever the converters already produced goes first.
        if self.pending_pos < self.pending_len {
            let n = (self.pending_len - self.pending_pos).min(out.len());
            out[..n].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
            self.pending_pos += n;
            if self.pending_pos == self.pending_len {
                self.pending_len = 0;
                self.pending_pos = 0;
            }
            return Ok((0, n));
        }
        if self.finished || out.is_empty() {
            return Ok((0, 0));
        }

        // Never read past the declared compressed size: the bytes after it are
        // padding and the check, not compressed data.
        let input = match self.limits.compressed_size {
            Some(c) => {
                // The end marker is inside the compressed size, so a block
                // that has consumed all of it without reaching the marker is
                // not the block its header describes. Saying so here matters:
                // the caller cannot see that the input it still holds is out
                // of bounds for this block, so it would otherwise offer those
                // bytes forever and never be told they cannot be used.
                if self.packed >= c {
                    return Err(XzErrorKind::SizeMismatch);
                }
                let left = usize::try_from(c - self.packed).unwrap_or(usize::MAX);
                &input[..input.len().min(left)]
            }
            None => input,
        };

        if self.converters.is_empty() {
            // The fast path: LZMA2 writes into the caller's buffer.
            let limit = self.room(out.len())?;
            // A block whose declared size has been produced still owes its
            // end-of-stream control byte, so LZMA2 is called once more with
            // no room and `FinishMode::End` to read it. C: `XzUnpacker_Code`
            // calls the coder with `outSize == 0` for exactly this.
            let finish = if limit == 0 {
                FinishMode::End
            } else {
                FinishMode::Any
            };
            let p = self.decode_lzma2(input, &mut out[..limit], finish)?;
            self.packed += p.read as u64;
            self.unpacked += p.written as u64;
            self.check.update(&out[..p.written]);
            self.note(p.status)?;
            if limit == 0 && !self.finished {
                return Err(self.at_capacity());
            }
            return Ok((p.read, p.written));
        }

        // C: liblzma simple_code / delta_decode: decode into caller output,
        // then convert in place. Only a destination too small for the held
        // tails needs private storage; twice the chain's maximum carry plus
        // one byte guarantees room for more input and a filtered prefix.
        // Keep the existing per-turn bound rather than filling an arbitrarily
        // large caller buffer. The LZMA2 dictionary remains unfiltered.
        let mut consumed = 0usize;
        loop {
            let start = self.converters.carried_bytes();
            let remaining = self.room(usize::MAX)?;
            // Carried bytes have already left LZMA2 but are not yet counted
            // as final output. They consume the same block-size allowance.
            if start > remaining {
                return Err(self.at_capacity());
            }
            let direct = out.len() > start;
            let capacity = if direct {
                out.len()
            } else {
                2 * self.converters.max_carry() + 1
            };
            let want = (remaining - start).min(SCRATCH).min(capacity - start);
            let finish = if want == 0 {
                FinishMode::End
            } else {
                FinishMode::Any
            };
            let (p, written) = if direct {
                let p =
                    self.decode_lzma2(&input[consumed..], &mut out[start..start + want], finish)?;
                let written = self.converters.decode_buffer(
                    out,
                    p.written,
                    p.status == Status::FinishedWithMark,
                );
                self.check.update(&out[..written]);
                (p, written)
            } else {
                let mut pending = core::mem::take(&mut self.pending);
                if pending.len() < capacity {
                    pending.reserve_exact(capacity - pending.len());
                    pending.resize(capacity, 0);
                }
                let decoded = self.decode_lzma2(
                    &input[consumed..],
                    &mut pending[start..start + want],
                    finish,
                );
                self.pending = pending;
                let p = decoded?;
                let written = self.converters.decode_buffer(
                    &mut self.pending,
                    p.written,
                    p.status == Status::FinishedWithMark,
                );
                self.check.update(&self.pending[..written]);
                (p, written)
            };
            consumed += p.read;
            self.packed += p.read as u64;
            self.unpacked += written as u64;
            self.note(p.status)?;
            // A held tail can exhaust the allowance before the end-marker
            // byte arrives. Let the reader refill; another data chunk is
            // still rejected by the zero-output FinishMode::End call.
            if want == 0 && !self.finished && p.status != Status::NeedsMoreInput {
                return Err(self.at_capacity());
            }
            if written != 0 || self.finished {
                if direct {
                    return Ok((consumed, written));
                }
                self.pending_len = written;
                let n = written.min(out.len());
                out[..n].copy_from_slice(&self.pending[..n]);
                self.pending_pos = n;
                if n == written {
                    self.pending_len = 0;
                    self.pending_pos = 0;
                }
                return Ok((consumed, n));
            }
            if p.read == 0 && p.written == 0 {
                return Ok((consumed, 0));
            }
            // A short filter tail may swallow all output without consuming
            // input. Continue until output, EOF or genuine lack of progress;
            // returning (0, 0) earlier would falsely request more input.
        }
    }

    /// One LZMA2 call, with its failure read for what it means here.
    ///
    /// With no room left the call exists only to read the end marker, and
    /// LZMA2 fails it when what follows is another chunk. That is a block
    /// running past its allowance, not corrupt data: reported as corruption, a
    /// caller that hit its own cap is told its file is broken.
    fn decode_lzma2(
        &mut self,
        input: &[u8],
        out: &mut [u8],
        finish: FinishMode,
    ) -> Result<crate::error::Progress, XzErrorKind> {
        let full = out.is_empty();
        self.lzma2.decode(input, out, finish).map_err(|error| {
            if full {
                self.at_capacity()
            } else {
                error.into()
            }
        })
    }

    /// The block has produced everything it is allowed to and has not stopped.
    fn at_capacity(&self) -> XzErrorKind {
        match self.limits.uncompressed_size {
            // The header said how big it was and it is bigger.
            Some(_) => XzErrorKind::SizeMismatch,
            // No declared size, so it is the caller's cap that ran out.
            None => XzErrorKind::TooMuchOutput {
                limit: self.limits.max_block_size,
            },
        }
    }

    /// How much output may still be produced, capped at `want`.
    fn room(&self, want: usize) -> Result<usize, XzErrorKind> {
        let cap = match self.limits.uncompressed_size {
            Some(n) => n,
            None => self.limits.max_block_size,
        };
        if self.unpacked > cap {
            return Err(XzErrorKind::TooMuchOutput { limit: cap });
        }
        Ok(usize::try_from(cap - self.unpacked)
            .unwrap_or(usize::MAX)
            .min(want))
    }

    fn note(&mut self, status: Status) -> Result<(), XzErrorKind> {
        if status == Status::FinishedWithMark {
            self.finished = true;
            if let Some(n) = self.limits.uncompressed_size
                && n != self.unpacked
            {
                return Err(XzErrorKind::SizeMismatch);
            }
            if let Some(c) = self.limits.compressed_size
                && c != self.packed
            {
                return Err(XzErrorKind::SizeMismatch);
            }
        }
        Ok(())
    }

    /// Closes the block against the `stored` bytes of its check field.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::CheckMismatch`] if what was decoded does not match.
    pub(crate) fn finish(self, stored: &[u8]) -> Result<BlockCheck, XzErrorKind> {
        self.check.finish(stored)
    }
}

#[cfg(test)]
mod tests {
    use super::super::filter::{FILTER_DELTA, FILTER_LZMA2, FilterFlags};
    use super::*;

    fn chain(id: u64, chained: bool) -> FilterChain {
        let mut flags = Vec::new();
        if chained {
            flags.push(FilterFlags::new(FILTER_DELTA, &[3]).unwrap());
            flags.push(FilterFlags::new(0x04, &[]).unwrap());
        }
        flags.push(FilterFlags::new(id, if id == FILTER_DELTA { &[63] } else { &[] }).unwrap());
        flags.push(FilterFlags::new(FILTER_LZMA2, &[0]).unwrap());
        FilterChain::validate(&flags).unwrap()
    }

    fn packed(chain: &FilterChain, original: &[u8]) -> Vec<u8> {
        let mut filtered = original.to_vec();
        chain.build().unwrap().encode_in_place(&mut filtered);
        let mut packed = Vec::new();
        // LZMA2 uncompressed chunks still exercise the real dictionary and
        // block state machine, without requiring an encoder feature.
        for (i, chunk) in filtered.chunks(65_536).enumerate() {
            packed.push(if i == 0 { 1 } else { 2 });
            packed.extend_from_slice(&((chunk.len() - 1) as u16).to_be_bytes());
            packed.extend_from_slice(chunk);
        }
        packed.push(0);
        packed
    }

    fn decoder(chain: &FilterChain, size: Option<u64>, cap: u64, packed: usize) -> BlockDecoder {
        BlockDecoder::new(
            chain,
            CheckType::Crc32,
            0,
            &ChecksumPlan::none(),
            true,
            BlockLimits {
                memory_limit: 1 << 20,
                uncompressed_size: size,
                compressed_size: Some(packed as u64),
                max_block_size: cap,
            },
        )
        .unwrap()
    }

    #[test]
    fn caller_output_filters_preserve_chains_tails_and_checks() {
        for id in FILTER_DELTA..=0x0b {
            for chained in [false, true] {
                let chain = chain(id, chained);
                for len in [0, 1, 3, 17, 257] {
                    let original: Vec<u8> = (0..len).map(|i| ((i * 37) ^ (i / 7)) as u8).collect();
                    let packed = packed(&chain, &original);
                    for width in [1, 2, 7, 19, 64, 65_536] {
                        for input_width in [1, 17] {
                            let mut dec =
                                decoder(&chain, Some(len as u64), len as u64, packed.len());
                            let mut pos = 0;
                            let mut got = Vec::new();
                            let mut out = alloc::vec![0xa5; width];
                            let mut turns = 0;
                            while !dec.finished() {
                                let end = packed.len().min(pos + input_width);
                                let (read, wrote) =
                                    dec.decode(&packed[pos..end], &mut out).unwrap();
                                pos += read;
                                got.extend_from_slice(&out[..wrote]);
                                assert!(read != 0 || wrote != 0 || dec.finished());
                                assert!(dec.pending.len() <= 2 * dec.converters.max_carry() + 1);
                                turns += 1;
                                assert!(turns <= 4 * (packed.len() + 1));
                            }
                            assert_eq!(pos, packed.len());
                            assert_eq!(
                                got, original,
                                "filter={id} chain={chained} len={len} out={width} in={input_width}"
                            );
                            let mut crc = crate::crc::Crc32::new();
                            crc.update(&original);
                            dec.finish(&crc.finalize().to_le_bytes()).unwrap();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn caller_output_filters_need_no_large_private_buffer() {
        for id in FILTER_DELTA..=0x0b {
            let chain = chain(id, true);
            let original: Vec<u8> = (0..65_557).map(|i| ((i * 37) ^ (i / 7)) as u8).collect();
            let packed = packed(&chain, &original);
            let mut dec = decoder(&chain, None, original.len() as u64, packed.len());
            let mut pos = 0;
            let mut got = Vec::new();
            let mut out = alloc::vec![0xa5; 1 << 20];
            while !dec.finished() {
                let (read, wrote) = dec.decode(&packed[pos..], &mut out).unwrap();
                pos += read;
                got.extend_from_slice(&out[..wrote]);
                assert!(read != 0 || wrote != 0 || dec.finished());
                assert_eq!(dec.pending.capacity(), 0);
                assert!(wrote <= SCRATCH + dec.converters.max_carry());
            }
            assert_eq!(got, original);
            assert_eq!(pos, packed.len());
            // A wrong block check is still rejected on the direct path.
            let mut crc = crate::crc::Crc32::new();
            crc.update(&original);
            assert!(matches!(
                dec.finish(&(crc.finalize() ^ 1).to_le_bytes()),
                Err(XzErrorKind::CheckMismatch)
            ));
        }
    }

    #[test]
    fn carried_filter_bytes_count_toward_the_block_limit() {
        let chain = chain(0x04, true);
        let original: Vec<u8> = (0..31).map(|i| i as u8).collect();
        let packed = packed(&chain, &original);
        for declared in [false, true] {
            for width in [1, 64] {
                let cap = original.len() as u64 - 1;
                let mut dec = decoder(&chain, declared.then_some(cap), cap, packed.len());
                let mut pos = 0;
                let mut delivered = 0;
                let mut out = alloc::vec![0xa5; width];
                let error = loop {
                    match dec.decode(&packed[pos..], &mut out) {
                        Ok((read, wrote)) => {
                            assert!(!dec.finished());
                            assert!(read != 0 || wrote != 0);
                            pos += read;
                            delivered += wrote;
                        }
                        Err(error) => break error,
                    }
                };
                assert!(delivered <= cap as usize);
                if declared {
                    assert!(matches!(error, XzErrorKind::SizeMismatch));
                } else {
                    assert!(matches!(error, XzErrorKind::TooMuchOutput { limit } if limit == cap));
                }
            }
        }
    }
}
