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
//! - `LZMA_TURBO_BCJ2`: `sdk` for the complete cursor kernel or `checked`
//!   (default) for the shipping slice decoder.
//! - `LZMA_TURBO_DELTA`: `scalar` or `block` (default).
//! - `LZMA_TURBO_TREE_KERNEL`: `slice` or `reg`, the bt thread's tree walk
//!   (the default is what the build ships: `reg` on x86-64 and ARM64 with `asm`).
//! - `LZMA_TURBO_BT_ASM`: which register walk the bt thread runs, when
//!   `LZMA_TURBO_TREE_KERNEL` leaves it on `reg`. Unset, or any value not
//!   listed for the target, selects the walk the build ships.
//!   - x86-64: `sdk` (default, what ships) for the complete upstream walk in
//!     `bt_kernel_x86_64_sdk.S`; `sdk-wide` for that flow with the bounded
//!     match extension; `earlier` for the superseded `bt_kernel_x86_64.S`.
//!   - macOS ARM64: `sdk-arm64-negative` (default, what ships) for the
//!     negative-offset port; `sdk-arm64` for the complete upstream
//!     instruction flow; `sdk-arm64-wide` for that flow with the bounded word
//!     scan; `portable` for the Rust pointer walk.
//!   - Other targets have no assembly walk and ignore the variable.
//! - `LZMA_TURBO_MIXER`: `sdk` for the SDK consumer's call boundary and
//!   position-relative addressing, or anything else (default) for the
//!   shipping consumer.
//! - `LZMA_TURBO_ENCODER`: `checked` for checked parser window reads and
//!   low-hash accesses, or `unchecked` (default).
//! - `LZMA_TURBO_FINDER`: `st` for the C's single-threaded match finder where
//!   one thread runs a binary tree in normal mode, or `inline` (default).
//! - `LZMA_TURBO_HC_HEADS`: `off` for the C's hash-chain finder, which looks
//!   each position's head up as it gets there, or `on` (default).
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

/// Whether the diagnostic BCJ2 decoder uses the complete SDK cursor port.
pub(crate) fn bcj2_sdk() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_BCJ2", |v| u8::from(v == "sdk")) == 1
}

/// Whether the delta decoder adds a block at a time at wide distances.
/// `false` is the C's byte-at-a-time recurrence.
pub(crate) fn delta_blocked() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_DELTA", |v| u8::from(v != "scalar")) == 1
}

/// Whether the bt thread walks the tree with the register-shaped kernel in
/// `enc::bt_kernel` (`true`) or the slice kernel in `enc::lz_find_mt`.
pub(crate) fn tree_kernel_reg() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_TREE_KERNEL", |v| match v {
        "slice" => 0,
        "reg" => 1,
        _ => u8::from(cfg!(all(
            feature = "asm",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ))),
    }) == 1
}

/// Diagnostic native walk selection; normal builds retain their fixed kernel.
/// Returns a `VARIANT` of `enc::bt_kernel::get_matches_spec_n_2_reg_variant`
/// (its table lists them), and its production variant when unset.
#[cfg(all(
    any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_os = "macos")
    ),
    not(miri)
))]
pub(crate) fn bt_variant() -> u8 {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_BT_ASM", bt_variant_of)
}

/// The `LZMA_TURBO_BT_ASM` mapping: distinct variants per value, and the
/// production variant for an unset or unknown value.
#[cfg(all(
    any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_os = "macos")
    ),
    not(miri)
))]
fn bt_variant_of(value: &str) -> u8 {
    const PRODUCTION: u8 = crate::enc::bt_kernel::PRODUCTION_VARIANT;
    if cfg!(target_arch = "x86_64") {
        match value {
            "earlier" => 0,
            "sdk-wide" => 2,
            _ => PRODUCTION,
        }
    } else {
        match value {
            "portable" => 0,
            "sdk-arm64" => 2,
            "sdk-arm64-wide" => 3,
            _ => PRODUCTION,
        }
    }
}

/// Diagnostic SDK consumer call boundary and position-relative addressing.
pub(crate) fn sdk_mixer() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_MIXER", |v| u8::from(v == "sdk")) == 1
}

/// Whether encoder window reads use the checked differential oracle.
pub(crate) fn encoder_checked() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_ENCODER", |v| u8::from(v == "checked")) == 1
}

/// Whether one thread runs the threaded match finder's stages inline. `false`
/// is the single-threaded finder the C takes there.
pub(crate) fn inline_finder() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_FINDER", |v| u8::from(v != "st")) == 1
}

/// Whether the hash-chain finder takes its heads a run of positions ahead.
/// `false` is the C's lookup at each position.
pub(crate) fn hc_heads() -> bool {
    static CACHED: AtomicU8 = AtomicU8::new(UNSET);
    cached(&CACHED, "LZMA_TURBO_HC_HEADS", |v| u8::from(v != "off")) == 1
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

#[cfg(all(
    test,
    any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_os = "macos")
    ),
    not(miri)
))]
mod tests {
    use super::bt_variant_of;
    use crate::enc::bt_kernel::PRODUCTION_VARIANT;

    #[test]
    fn every_bt_asm_value_selects_its_own_walk_and_unset_selects_production() {
        let (shipped, others): (&str, &[&str]) = if cfg!(target_arch = "x86_64") {
            ("sdk", &["earlier", "sdk-wide"])
        } else {
            (
                "sdk-arm64-negative",
                &["portable", "sdk-arm64", "sdk-arm64-wide"],
            )
        };
        assert_eq!(bt_variant_of(""), PRODUCTION_VARIANT);
        assert_eq!(bt_variant_of("no-such-walk"), PRODUCTION_VARIANT);
        assert_eq!(bt_variant_of(shipped), PRODUCTION_VARIANT);
        let mut seen = vec![PRODUCTION_VARIANT];
        for value in others {
            let variant = bt_variant_of(value);
            assert!(!seen.contains(&variant), "{value} shares variant {variant}");
            seen.push(variant);
        }
    }
}
