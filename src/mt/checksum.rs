//! Checksums computed where the bytes are, not where they are written.
//!
//! C: nothing. `Lzma2DecMt.c` decodes and writes; 7-Zip checksums a folder's
//! sub-streams afterwards, in `CFolderOutStream`, on the thread that consumes
//! the output.
//!
//! That is the trap this module exists to close. In the `MtDec` ring the
//! write callback is the only serialised section - one thread holds the write
//! token at a time - so a checksum computed by the consumer as it receives the
//! output is not merely serial, it is *inside* the decoder's critical section,
//! and it costs the decode both its own time and the queueing it induces on
//! every other worker. Measuring this crate against 7-Zip found exactly that:
//! a table-driven CRC-32 in the benchmark's sink cost 16% of an eight-thread
//! decode and turned parity into a 10% deficit (see `docs/perf-log.md`).
//!
//! So the decoder offers to do it instead, in the worker, over the block that
//! worker just produced, before it queues for the write token. Two shapes:
//!
//! - **CRC-32 and CRC-64/XZ** are computed per *segment*. A consumer's own
//!   boundaries - the files of a 7z folder, say - do not line up with the
//!   decoder's blocks, so the caller hands in the absolute unpacked offsets
//!   where its boundaries fall and each worker emits one checksum per piece of
//!   its block between them, in a single pass over the bytes. The pieces are
//!   then folded into whatever ranges the consumer actually cares about with
//!   [`crate::crc::CrcFolder`], which never re-reads a byte.
//! - **SHA-256** is not foldable, so it is computed over the whole block only
//!   and split points are ignored for it. A consumer that needs SHA-256 over
//!   an arbitrary range has to hash that range itself, on its own thread; what
//!   the decoder can do for free is the per-block digest, which is what an xz
//!   stream with check type 10 needs, because there an xz block *is* the unit
//!   being checked.

use alloc::sync::Arc;
use alloc::vec::Vec;

/// Which checksum the decoder should compute for each block it produces.
///
/// The three are the check types the containers around LZMA use: CRC-32 is the
/// 7z per-file checksum and xz check type 1, CRC-64/XZ is xz check type 4 and
/// its default, and SHA-256 is xz check type 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Checksum {
    /// Compute nothing. The decoder does exactly what it did before.
    #[default]
    None,
    /// CRC-32/ISO-HDLC, per segment, foldable with [`crate::crc::crc32_combine`].
    Crc32,
    /// CRC-64/XZ, per segment, foldable with [`crate::crc::crc64_xz_combine`].
    Crc64Xz,
    /// SHA-256 of each whole block. Not foldable; split points are ignored.
    #[cfg(any(feature = "crypto", feature = "native-crypto"))]
    Sha256,
}

/// The checksum of one segment, at whichever width was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentCheck {
    /// CRC-32/ISO-HDLC.
    Crc32(u32),
    /// CRC-64/XZ.
    Crc64(u64),
}

impl SegmentCheck {
    /// The CRC-32 value, or `None` if this is the other width.
    #[must_use]
    pub fn crc32(self) -> Option<u32> {
        match self {
            SegmentCheck::Crc32(v) => Some(v),
            SegmentCheck::Crc64(_) => None,
        }
    }

    /// The CRC-64/XZ value, or `None` if this is the other width.
    #[must_use]
    pub fn crc64(self) -> Option<u64> {
        match self {
            SegmentCheck::Crc64(v) => Some(v),
            SegmentCheck::Crc32(_) => None,
        }
    }
}

/// One piece of decoded output and its checksum.
///
/// `offset` is absolute in the *unpacked* stream, so segments from different
/// blocks - and from different threads - are directly comparable and can be
/// pushed into a [`crate::crc::CrcFolder`] in any order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Absolute offset of the first byte, in the decoded stream.
    pub offset: u64,
    /// Length in bytes. Never zero.
    pub len: u64,
    /// The checksum of those bytes.
    pub check: SegmentCheck,
}

/// What one worker computed for one output block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockChecks {
    /// Absolute offset of the block's first byte in the decoded stream.
    pub unpacked_offset: u64,
    /// The block's length in bytes.
    pub len: u64,
    /// SHA-256 of the whole block, if [`Checksum::Sha256`] was asked for.
    pub digest: Option<[u8; 32]>,
    /// One entry per piece of the block between split points, if a CRC was
    /// asked for. With no split points inside the block this is one entry
    /// covering all of it.
    pub segments: Vec<Segment>,
}

/// What to compute, and where the consumer's boundaries fall.
///
/// Cheap to clone: the split points are shared, not copied, because every
/// worker needs them.
#[derive(Debug, Clone, Default)]
pub struct ChecksumPlan {
    kind: Checksum,
    splits: Option<Arc<[u64]>>,
}

impl ChecksumPlan {
    /// Compute nothing.
    #[must_use]
    pub fn none() -> Self {
        ChecksumPlan::default()
    }

    /// Compute `kind` over each whole block, with no split points.
    #[must_use]
    pub fn new(kind: Checksum) -> Self {
        ChecksumPlan { kind, splits: None }
    }

    /// Sets the absolute unpacked offsets at which segments are to be cut.
    ///
    /// Offsets are sorted and deduplicated here, so a caller may pass them in
    /// any order. An offset of 0, or one at or past the end of the stream, has
    /// no effect: a split point only ever divides bytes that exist. Ignored
    /// for [`Checksum::Sha256`], which is not foldable.
    #[must_use]
    pub fn with_split_points<I: IntoIterator<Item = u64>>(mut self, points: I) -> Self {
        let mut v: Vec<u64> = points.into_iter().filter(|&p| p != 0).collect();
        v.sort_unstable();
        v.dedup();
        self.splits = if v.is_empty() {
            None
        } else {
            Some(Arc::from(v.into_boxed_slice()))
        };
        self
    }

    /// What is to be computed.
    #[must_use]
    pub fn kind(&self) -> Checksum {
        self.kind
    }

    /// The split points, sorted and deduplicated.
    #[must_use]
    pub fn split_points(&self) -> &[u64] {
        self.splits.as_deref().unwrap_or(&[])
    }

    /// Whether this plan asks for any work at all.
    #[must_use]
    pub(crate) fn is_none(&self) -> bool {
        self.kind == Checksum::None
    }
}

// ---------------------------------------------------------------------------
// The worker's half
// ---------------------------------------------------------------------------

/// The running checksum of the segment currently open.
enum Running {
    Crc32(crate::crc::Crc32),
    Crc64(crate::crc::Crc64Xz),
}

impl Running {
    fn start(kind: Checksum) -> Option<Self> {
        match kind {
            Checksum::Crc32 => Some(Running::Crc32(crate::crc::Crc32::new())),
            Checksum::Crc64Xz => Some(Running::Crc64(crate::crc::Crc64Xz::new())),
            _ => None,
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Running::Crc32(d) => d.update(data),
            Running::Crc64(d) => d.update(data),
        }
    }

    fn finish(self) -> SegmentCheck {
        match self {
            Running::Crc32(d) => SegmentCheck::Crc32(d.finalize()),
            Running::Crc64(d) => SegmentCheck::Crc64(d.finalize()),
        }
    }
}

/// Checksums a run of bytes as it is produced, cutting segments at the plan's
/// split points.
///
/// Fed by the worker over its whole block in one call, and by the
/// single-threaded path piece by piece as it writes; both give the same
/// answer, which is the property [`Segmenter`] exists to guarantee.
pub(crate) struct Segmenter {
    kind: Checksum,
    splits: Option<Arc<[u64]>>,
    /// Index of the first split point strictly after [`Self::pos`].
    next_split: usize,
    /// Absolute offset where the open segment began.
    seg_start: u64,
    /// Absolute offset of the next byte to be fed.
    pos: u64,
    /// Absolute offset where this run began.
    start: u64,
    running: Option<Running>,
    #[cfg(any(feature = "crypto", feature = "native-crypto"))]
    sha: Option<crate::crypto::Sha256>,
    segments: Vec<Segment>,
}

impl Segmenter {
    /// A segmenter for a run beginning at absolute unpacked offset `start`.
    pub(crate) fn new(plan: &ChecksumPlan, start: u64) -> Self {
        let splits = plan.splits.clone();
        let next_split = splits
            .as_deref()
            .map_or(0, |s| s.partition_point(|&p| p <= start));
        Segmenter {
            kind: plan.kind,
            splits,
            next_split,
            seg_start: start,
            pos: start,
            start,
            running: Running::start(plan.kind),
            #[cfg(any(feature = "crypto", feature = "native-crypto"))]
            sha: match plan.kind {
                Checksum::Sha256 => Some(crate::crypto::Sha256::new()),
                _ => None,
            },
            segments: Vec::new(),
        }
    }

    /// Reserves an already-bounded run's segment records before decoding.
    pub(crate) fn reserve_segments(&mut self, count: usize) -> Result<(), crate::Error> {
        self.segments
            .try_reserve_exact(count)
            .map_err(|_| crate::Error::Alloc)
    }

    /// Feeds the next bytes of the run. One pass: every byte is looked at
    /// once, whatever the split points do.
    pub(crate) fn update(&mut self, mut data: &[u8]) {
        #[cfg(any(feature = "crypto", feature = "native-crypto"))]
        if let Some(sha) = self.sha.as_mut() {
            sha.update(data);
        }
        if self.running.is_none() {
            self.pos += data.len() as u64;
            return;
        }
        while !data.is_empty() {
            // How far can we go before the next cut?
            let cut = self
                .splits
                .as_deref()
                .and_then(|s| s.get(self.next_split).copied())
                .filter(|&c| c < self.pos + data.len() as u64);
            let take = match cut {
                Some(c) => (c - self.pos) as usize,
                None => data.len(),
            };
            let (head, tail) = data.split_at(take);
            if let Some(r) = self.running.as_mut() {
                r.update(head);
            }
            self.pos += take as u64;
            data = tail;
            if cut.is_some() {
                self.next_split += 1;
                // `take` is 0 only when a split point lands exactly where the
                // open segment began, and then there is nothing to close.
                if self.pos != self.seg_start {
                    self.close();
                }
                self.running = Running::start(self.kind);
                self.seg_start = self.pos;
            }
        }
    }

    /// Absolute offset of the next byte this segmenter expects.
    pub(crate) fn pos(&self) -> u64 {
        self.pos
    }

    /// Closes the open segment, which must be non-empty.
    fn close(&mut self) {
        if let Some(r) = self.running.take() {
            self.segments.push(Segment {
                offset: self.seg_start,
                len: self.pos - self.seg_start,
                check: r.finish(),
            });
        }
        self.seg_start = self.pos;
    }

    /// Closes the run and returns what was computed for it.
    pub(crate) fn finish(mut self) -> BlockChecks {
        if self.pos != self.seg_start {
            self.close();
        }
        BlockChecks {
            unpacked_offset: self.start,
            len: self.pos - self.start,
            #[cfg(any(feature = "crypto", feature = "native-crypto"))]
            digest: self.sha.map(crate::crypto::Sha256::finalize),
            #[cfg(not(any(feature = "crypto", feature = "native-crypto")))]
            digest: None,
            segments: self.segments,
        }
    }
}
