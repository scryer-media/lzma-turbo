//! Verifying a block's check, in the decoder that produced the block.
//!
//! C: `XzCheck_Update` / `XzCheck_Final` in `XzDec.c`, which run on the
//! decoding thread. This does too - and it is the same machinery the LZMA2
//! workers already use ([`crate::checksum`]), so a block decoded by a worker
//! is checked by that worker and never on the thread draining the output.
//!
//! Because the caller may also want the decoded bytes cut at boundaries of its
//! own, the check is computed as *segments* and folded into the block's single
//! value with [`crate::crc::CrcFolder`]. One pass over the bytes either way:
//! the fold is arithmetic on the segment values, not a re-read.

use alloc::vec::Vec;

use super::error::XzErrorKind;
use super::stream::CheckType;
use crate::crc::CrcFolder;
use crate::mt::checksum::{BlockChecks, Checksum, ChecksumPlan, Segment, Segmenter};

/// The check of one block, as produced by whoever decoded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockCheck {
    /// Absolute offset of the block's first byte in the decoded stream.
    pub unpacked_offset: u64,
    /// The block's length in bytes.
    pub len: u64,
    /// The pieces the caller's split points cut the block into, with their
    /// CRCs. Empty for a stream with no check, for a SHA-256 stream, and for
    /// a decode that was not asked to verify.
    pub segments: Vec<Segment>,
    /// SHA-256 of the whole block, for a check type 10 stream.
    pub digest: Option<[u8; 32]>,
}

/// A check being computed over a block as it decodes.
pub(crate) struct RunningCheck {
    kind: CheckType,
    seg: Option<Segmenter>,
    start: u64,
}

impl RunningCheck {
    /// A check for a block starting at unpacked offset `start`.
    ///
    /// `splits` are the caller's absolute boundaries; they only ever refine
    /// the block's own single value, which is recovered by folding.
    pub(crate) fn new(kind: CheckType, start: u64, splits: &ChecksumPlan, verify: bool) -> Self {
        let plan_kind = match kind {
            CheckType::Crc32 => Checksum::Crc32,
            CheckType::Crc64 => Checksum::Crc64Xz,
            #[cfg(any(feature = "crypto", feature = "native-crypto"))]
            CheckType::Sha256 => Checksum::Sha256,
            #[cfg(not(any(feature = "crypto", feature = "native-crypto")))]
            CheckType::Sha256 => Checksum::None,
            CheckType::None | CheckType::Reserved(_) => Checksum::None,
        };
        let seg = if verify && plan_kind != Checksum::None {
            // The caller's split points are already sorted and deduplicated;
            // they are shared, not rebuilt, for every block.
            Some(Segmenter::new(&splits.with_kind(plan_kind), start))
        } else {
            None
        };
        RunningCheck { kind, seg, start }
    }

    /// Feeds the next decoded bytes of the block.
    pub(crate) fn update(&mut self, data: &[u8]) {
        if let Some(s) = self.seg.as_mut() {
            s.update(data);
        }
    }

    /// Closes the block and compares what was computed with the `stored`
    /// bytes of the check field.
    ///
    /// Returns the block's segments and digest for the caller.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::CheckMismatch`] if the values differ.
    pub(crate) fn finish(self, stored: &[u8]) -> Result<BlockCheck, XzErrorKind> {
        let Some(seg) = self.seg else {
            return Ok(BlockCheck {
                unpacked_offset: self.start,
                len: 0,
                segments: Vec::new(),
                digest: None,
            });
        };
        let BlockChecks {
            unpacked_offset,
            len,
            digest,
            segments,
        } = seg.finish();

        match self.kind {
            CheckType::Crc32 => {
                let mut f = CrcFolder::<u32>::new();
                for s in &segments {
                    if let Some(v) = s.check.crc32() {
                        f.push(s.offset, s.len, v);
                    }
                }
                let have = f
                    .range(unpacked_offset, len)
                    .ok_or(XzErrorKind::CheckMismatch)?;
                let want = u32::from_le_bytes(
                    stored
                        .get(..4)
                        .and_then(|s| s.try_into().ok())
                        .ok_or(XzErrorKind::TruncatedInput)?,
                );
                if have != want {
                    return Err(XzErrorKind::CheckMismatch);
                }
            }
            CheckType::Crc64 => {
                let mut f = CrcFolder::<u64>::new();
                for s in &segments {
                    if let Some(v) = s.check.crc64() {
                        f.push(s.offset, s.len, v);
                    }
                }
                let have = f
                    .range(unpacked_offset, len)
                    .ok_or(XzErrorKind::CheckMismatch)?;
                let want = u64::from_le_bytes(
                    stored
                        .get(..8)
                        .and_then(|s| s.try_into().ok())
                        .ok_or(XzErrorKind::TruncatedInput)?,
                );
                if have != want {
                    return Err(XzErrorKind::CheckMismatch);
                }
            }
            CheckType::Sha256 => {
                let have = digest.ok_or(XzErrorKind::UnsupportedCheck)?;
                let want = stored.get(..32).ok_or(XzErrorKind::TruncatedInput)?;
                if have != want {
                    return Err(XzErrorKind::CheckMismatch);
                }
            }
            CheckType::None | CheckType::Reserved(_) => {}
        }

        Ok(BlockCheck {
            unpacked_offset,
            len,
            segments,
            digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc::Crc64Xz;

    fn plan() -> ChecksumPlan {
        ChecksumPlan::new(Checksum::Crc32).with_split_points([40, 7, 0, 7, 100, 3])
    }

    #[test]
    fn every_block_shares_the_callers_split_table() {
        let plan = plan();
        let table = plan.split_points().as_ptr();
        for start in [0, 5, 39, 64, 1000] {
            let running = RunningCheck::new(CheckType::Crc64, start, &plan, true);
            let seg = running.seg.as_ref().expect("a verified CRC-64 block");
            assert_eq!(seg.split_points().as_ptr(), table, "block at {start}");
            assert_eq!(seg.split_points(), plan.split_points());
        }
    }

    #[test]
    fn shared_split_table_gives_the_same_segments_and_check() {
        let plan = plan();
        let data: Vec<u8> = (0..120u32).map(|i| (i * 31 % 251) as u8).collect();
        for start in [0u64, 5, 39] {
            // The plan the old code rebuilt for every block.
            let rebuilt = ChecksumPlan::new(Checksum::Crc64Xz)
                .with_split_points(plan.split_points().iter().copied());
            let mut old = Segmenter::new(&rebuilt, start);
            old.update(&data);
            let old = old.finish();

            let mut running = RunningCheck::new(CheckType::Crc64, start, &plan, true);
            running.update(&data[..13]);
            running.update(&data[13..]);
            let mut crc = Crc64Xz::new();
            crc.update(&data);
            let got = running.finish(&crc.finalize().to_le_bytes()).unwrap();
            assert_eq!(got.segments, old.segments);
            assert_eq!(got.unpacked_offset, old.unpacked_offset);
            assert_eq!(got.len, old.len);
        }
    }
}
