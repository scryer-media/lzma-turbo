//! Run-time arm selection for the kernels, for benchmarking only.
//!
//! The whole module is behind the `kernel-ab` feature, which is not default
//! and which nothing in the crate turns on. It exists so that an A/B of a
//! kernel against the loop it replaced is *one binary*: the arm is a branch on
//! a value loaded once and cached, sitting exactly where a cached
//! `is_x86_feature_detected` would sit, rather than two builds whose inlining,
//! layout and code placement differ for reasons that have nothing to do with
//! the kernel. Two builds cannot be compared at the couple of percent the
//! acceptance gate is written in.
//!
//! Without the feature none of this is compiled, the kernels have no branch,
//! and the crate reads no environment variables.
//!
//! - `LZMA_TURBO_MATCH_RUN`: `scalar`, `w8` (default) or `w16`.
//! - `LZMA_TURBO_BCJ_SCAN`: `scalar` or `wide` (default).
//! - `LZMA_TURBO_DELTA`: `scalar` or `block` (default).
//! - `LZMA_TURBO_FINDER`: `st` for the C's single-threaded match finder where
//!   one thread runs a binary tree in normal mode, or `inline` (default).
//!
//! The numbers these produced are in `docs/simd-kernels-report.md`.

use core::sync::atomic::{AtomicU8, Ordering};

/// Which arm the match-extension scan takes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchRunArm {
    /// The SDK's byte loop.
    Scalar,
    /// Eight bytes at a time. What ships.
    Word8,
    /// Sixteen bytes at a time.
    Word16,
}

/// Reads `LZMA_TURBO_MATCH_RUN` once and caches it.
pub(crate) fn match_run_arm() -> MatchRunArm {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    match cached(&CACHED, "LZMA_TURBO_MATCH_RUN", |v| match v {
        "scalar" => 0,
        "w16" => 2,
        _ => 1,
    }) {
        0 => MatchRunArm::Scalar,
        2 => MatchRunArm::Word16,
        _ => MatchRunArm::Word8,
    }
}

/// Whether the x86 branch-byte search runs a word at a time. `false` is the
/// SDK's four-byte-with-four-tests loop.
pub(crate) fn bcj_scan_wide() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_BCJ_SCAN", |v| u8::from(v != "scalar")) == 1
}

/// Whether the delta decoder adds a block at a time at wide distances.
/// `false` is the C's byte-at-a-time recurrence.
pub(crate) fn delta_blocked() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_DELTA", |v| u8::from(v != "scalar")) == 1
}

/// Whether one thread runs the threaded match finder's stages inline. `false`
/// is the single-threaded finder the C takes there.
pub(crate) fn inline_finder() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_FINDER", |v| u8::from(v != "st")) == 1
}

const UNSET: u8 = 255;

/// The load-once-and-cache the two above share. A relaxed load is enough: the
/// value is the same whoever computes it, so a race only costs a second
/// `var_os`, and the toggle is read outside the loops it selects.
fn cached(slot: &AtomicU8, var: &str, parse: fn(&str) -> u8) -> u8 {
    let seen = slot.load(Ordering::Relaxed);
    if seen != UNSET {
        return seen;
    }
    let chosen = std::env::var(var).map_or_else(|_| parse(""), |v| parse(&v));
    slot.store(chosen, Ordering::Relaxed);
    chosen
}
