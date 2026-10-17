//! The `.xz` container.
//!
//! Ported from 7-Zip's `C/XzDec.c`, `C/XzIn.c` and `C/Xz.c` (public domain),
//! and checked against the xz file format specification version 1.2.1. Each
//! module names the C function it came from.
//!
//! The layers, outermost first:
//!
//! - a **file** is one or more streams, each optionally followed by stream
//!   padding ([`XzReader`] reads them all by default, as `xz -d` does);
//! - a **stream** is a header, blocks, an index and a footer ([`stream`],
//!   [`index`]);
//! - a **block** is a header, compressed data, padding and a check
//!   ([`block`], [`check`]);
//! - the compressed data is an **LZMA2 stream** with up to three converters
//!   in front of it ([`filter`], [`bcj`], [`delta`]).
//!
//! # Example
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::Read;
//! use lzma_turbo::xz::XzReader;
//!
//! # fn main() -> std::io::Result<()> {
//! let mut out = Vec::new();
//! XzReader::new(File::open("archive.tar.xz")?).read_to_end(&mut out)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Limits
//!
//! Everything this module allocates is bounded before it is allocated, and
//! every bound is a documented one; see `docs/security.md`. The two a caller
//! usually sets are [`XzOptions::memory_limit`], which caps what one block's
//! dictionary may cost, and [`XzOptions::max_unpack_bytes`], which caps the
//! output of a whole file.

pub mod block;
pub mod check;
pub mod error;
pub mod filter;
pub mod index;
pub mod stream;
pub mod vli;

/// The converters live in [`crate::filters`], behind the `filters` feature
/// that `xz` implies; these are the paths they have always had.
pub use crate::filters::{bcj, delta};

mod adaptive;
mod blockdec;
mod parallel;
mod pool;
mod reader;

pub use adaptive::XzAdaptiveDecoder;
pub use block::{BlockHeader, MAX_BLOCK_HEADER_SIZE};
pub use check::BlockCheck;
pub use error::{XzError, XzErrorKind, XzResult};
pub use filter::{FILTER_DELTA, FILTER_LZMA2, FilterChain, FilterFlags, MAX_FILTERS};
pub use index::{
    MAX_INDEX_SIZE, XzBlockEntry, XzIndex, XzIndexRecord, XzStreamIndex, block_table,
    is_single_stream_multi_block, read_stream_index_ending_at, single_stream_block_count,
    stream_table,
};
pub use parallel::XzParallelReader;
pub use reader::XzReader;
pub use stream::{CheckType, StreamFlags, StreamFooter, StreamHeader, probe};

use crate::mt::checksum::ChecksumPlan;

/// How much output a single block may produce when nothing else caps it.
///
/// A block header need not declare an uncompressed size, so a hostile stream
/// can otherwise ask a decoder to produce output forever. 64 GiB is far above
/// any block `xz` writes (its largest preset block is 768 MiB) and far below
/// anything that could run a machine out of disk unnoticed.
pub const DEFAULT_MAX_BLOCK_SIZE: u64 = 64 << 30;

/// The default memory limit: none.
///
/// A limit is the caller's policy, not the format's, so the default is not to
/// have one. Callers that decode untrusted input should set
/// [`XzOptions::memory_limit`].
pub const DEFAULT_MEMORY_LIMIT: u64 = u64::MAX;

/// What a decode is allowed to do.
///
/// Construct with [`XzOptions::default`] and set the fields that matter; the
/// struct is `#[non_exhaustive]` so that later versions can add limits without
/// breaking callers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct XzOptions {
    /// Worker threads for the parallel paths. `0` asks for one per available
    /// core. Ignored by [`XzReader`], which is single-threaded by
    /// construction.
    pub threads: usize,
    /// The most memory one block's dictionary may cost, in bytes. A stream
    /// that needs more fails with [`XzErrorKind::MemoryLimit`] rather than
    /// allocating it.
    pub memory_limit: u64,
    /// Whether to compute and compare each block's check. Turning it off
    /// decodes a stream whose check this build cannot compute, and saves the
    /// checksum's time; it also means corruption is only caught by the
    /// compressed data failing to decode.
    pub verify_checks: bool,
    /// Whether a stream whose check type this build cannot compute may be
    /// decoded unverified. Without this, such a stream is an error, so that a
    /// caller never silently gets unchecked bytes.
    pub allow_unverifiable: bool,
    /// A cap on what one block may decode to, whether or not its header says.
    pub max_block_size: u64,
    /// A cap on what the whole file may decode to.
    pub max_unpack_bytes: Option<u64>,
    /// Whether to keep reading after the first stream ends. `xz -d` does, and
    /// so does this by default; turning it off makes trailing bytes an error.
    pub concatenated: bool,
    /// Where the caller wants the decoded output cut for its own checksums.
    /// The decoder computes these in the same pass as the block's own check.
    pub plan: ChecksumPlan,
    /// Whether [`XzParallelReader`] holds each block until its check has
    /// passed, so that no byte of a corrupt block is ever returned, at every
    /// thread count.
    ///
    /// With more than one thread the parallel reader decodes whole blocks
    /// anyway, and always holds them. With one it does so only when this is
    /// set, at the cost of a buffer the size of the largest block. Unset, the
    /// default, a one-thread decode runs as [`XzReader`] does, in the
    /// block's dictionary: a block's bytes are returned as they are decoded,
    /// and a failed check is reported at the block's end. Ignored by
    /// [`XzReader`], which always works that way.
    pub verified_output: bool,
}

impl Default for XzOptions {
    fn default() -> Self {
        XzOptions {
            threads: 0,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            verify_checks: true,
            allow_unverifiable: false,
            max_block_size: DEFAULT_MAX_BLOCK_SIZE,
            max_unpack_bytes: None,
            concatenated: true,
            plan: ChecksumPlan::none(),
            verified_output: false,
        }
    }
}

impl XzOptions {
    /// Sets [`XzOptions::memory_limit`].
    #[must_use]
    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.memory_limit = bytes;
        self
    }

    /// Sets [`XzOptions::verify_checks`].
    #[must_use]
    pub fn with_checks(mut self, verify: bool) -> Self {
        self.verify_checks = verify;
        self
    }

    /// Sets [`XzOptions::threads`].
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads;
        self
    }

    /// Sets [`XzOptions::concatenated`].
    #[must_use]
    pub fn with_concatenated(mut self, on: bool) -> Self {
        self.concatenated = on;
        self
    }

    /// Sets [`XzOptions::max_unpack_bytes`].
    #[must_use]
    pub fn with_max_unpack_bytes(mut self, cap: Option<u64>) -> Self {
        self.max_unpack_bytes = cap;
        self
    }

    /// Sets [`XzOptions::plan`].
    #[must_use]
    pub fn with_plan(mut self, plan: ChecksumPlan) -> Self {
        self.plan = plan;
        self
    }

    /// Sets [`XzOptions::verified_output`].
    #[must_use]
    pub fn with_verified_output(mut self, on: bool) -> Self {
        self.verified_output = on;
        self
    }
}
