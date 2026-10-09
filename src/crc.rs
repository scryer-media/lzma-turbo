//! The checksums the container formats around LZMA use.
//!
//! C: `C/7zCrc.c` (CRC-32, the 7z checksum) and `C/XzCrc64.c` (CRC-64/XZ,
//! the xz one). Neither is used by the decoder: they belong to the layer that
//! reads a container and verifies what came out of it.
//!
//! Both are carry-less-multiply implementations from [`crc_fast`], which is
//! the one library dependency this crate takes for them. Requires the `crc`
//! feature.
//!
//! # Delegating the bulk checksum on wasm
//!
//! With the `crc-host` feature on a `wasm32` target, [`Crc32`], [`Crc64Xz`]
//! and the one-shot [`crc32`] / [`crc64_xz`] compute nothing themselves: each
//! carries a running checksum in a plain integer and folds bytes into it
//! through the embedder's `crc32` / `crc64_xz` hooks (see [`crate::hooks`]).
//! wasm has no carry-less multiply instruction, so `crc-fast`'s whole reason
//! for existing is absent there, while a host that has one can checksum an xz
//! block far faster than the guest can.
//!
//! Nothing about the API changes: the types, their methods and the free
//! functions are the same on every target, so every caller - the xz readers,
//! `crate::xz::check`, the multi-threaded checksum planner - picks the
//! delegation up without a line of change. The hooks' seeded-resume contract,
//! `C(C(0, a), b) == C(0, a ++ b)`, is exactly what makes a running integer a
//! faithful stand-in for a streaming digest; [`crate::hooks`] states it in
//! full and the tests at the bottom of this module prove it holds through the
//! real registry.
//!
//! [`crc32_combine`] and [`crc64_xz_combine`] are not delegated on any target.
//! Folding is arithmetic on two checksums and a length, never a pass over
//! data, so crossing a boundary for it would be pure overhead.
//!
//! On a native target `crc-host` is accepted and inert: the `crc-fast`
//! implementations stay active and no hook is ever called.

use crc_fast::CrcAlgorithm;
#[cfg(not(all(target_arch = "wasm32", feature = "crc-host")))]
use crc_fast::Digest;

/// Fold `data` into a running CRC-32 through the embedder-installed hook.
///
/// Dead on a native target outside `#[cfg(test)]`: only a wasm build has a
/// host to delegate to. The tests still drive it directly, so the hook path is
/// proven without a wasm runtime.
#[cfg(feature = "crc-host")]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[inline]
fn crc32_host(seed: u32, data: &[u8]) -> u32 {
    (crate::hooks::hooks().crc32)(seed, data)
}

/// Fold `data` into a running CRC-64/XZ through the embedder-installed hook.
#[cfg(feature = "crc-host")]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[inline]
fn crc64_xz_host(seed: u64, data: &[u8]) -> u64 {
    (crate::hooks::hooks().crc64_xz)(seed, data)
}

/// Emits one checksum width twice over: the host-delegated running-integer
/// form for a wasm build with `crc-host`, and the `crc_fast::Digest` wrapper
/// for every other build. Exactly one of the two compiles, and the two present
/// the same API down to the `Default` impl and the one-shot free function.
macro_rules! digest {
    ($name:ident, $algo:expr, $out:ty, $one:ident, $hook:ident, $ty_doc:literal, $doc:literal) => {
        #[doc = $ty_doc]
        ///
        /// This build delegates: the value is a running checksum in the
        /// finalized domain, seeded at 0 and advanced by the embedder's hook.
        #[cfg(all(target_arch = "wasm32", feature = "crc-host"))]
        #[derive(Debug)]
        pub struct $name($out);

        #[cfg(all(target_arch = "wasm32", feature = "crc-host"))]
        impl $name {
            /// A digest over no bytes yet.
            #[must_use]
            pub fn new() -> Self {
                Self(0)
            }

            /// Feeds the next bytes of the stream.
            pub fn update(&mut self, data: &[u8]) {
                if data.is_empty() {
                    return;
                }
                self.0 = $hook(self.0, data);
            }

            /// Consumes the digest and returns the checksum.
            #[must_use]
            pub fn finalize(self) -> $out {
                self.0
            }
        }

        #[doc = $ty_doc]
        #[cfg(not(all(target_arch = "wasm32", feature = "crc-host")))]
        #[derive(Debug)]
        pub struct $name(Digest);

        #[cfg(not(all(target_arch = "wasm32", feature = "crc-host")))]
        impl $name {
            /// A digest over no bytes yet.
            #[must_use]
            pub fn new() -> Self {
                Self(Digest::new($algo))
            }

            /// Feeds the next bytes of the stream.
            pub fn update(&mut self, data: &[u8]) {
                self.0.update(data);
            }

            /// Consumes the digest and returns the checksum.
            #[must_use]
            pub fn finalize(self) -> $out {
                self.0.finalize() as $out
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        #[doc = $doc]
        ///
        /// Delegated to the embedder's hook on a wasm build with `crc-host`,
        /// as a single seeded-from-zero call.
        #[cfg(all(target_arch = "wasm32", feature = "crc-host"))]
        #[must_use]
        pub fn $one(data: &[u8]) -> $out {
            $hook(0, data)
        }

        #[doc = $doc]
        #[cfg(not(all(target_arch = "wasm32", feature = "crc-host")))]
        #[must_use]
        pub fn $one(data: &[u8]) -> $out {
            crc_fast::checksum($algo, data) as $out
        }
    };
}

digest!(
    Crc32,
    CrcAlgorithm::Crc32IsoHdlc,
    u32,
    crc32,
    crc32_host,
    "CRC-32 (IEEE, reflected), the checksum in `.7z` headers and streams.\n\nC: `CrcCalc` in `C/7zCrc.c`.",
    "CRC-32 of one contiguous buffer."
);
digest!(
    Crc64Xz,
    CrcAlgorithm::Crc64Xz,
    u64,
    crc64_xz,
    crc64_xz_host,
    "CRC-64/XZ (ECMA-182, reflected, with the xz initial and final xor), the checksum an `.xz` block or index carries.\n\nC: `Crc64Calc` in `C/XzCrc64.c`.",
    "CRC-64/XZ of one contiguous buffer."
);

// ---------------------------------------------------------------------------
// Folding
// ---------------------------------------------------------------------------

/// A CRC width that can be folded: given the checksums of two adjacent ranges
/// and the length of the second, the checksum of the two concatenated.
///
/// This is what makes a CRC usable from a decoder that produces its output out
/// of order, or on several threads at once: each worker checksums only the
/// bytes it produced, and the consumer folds the pieces together afterwards
/// without ever re-reading them. A cryptographic digest has no such operation,
/// which is why [`crate::crypto::Sha256`] is not foldable and the multi-threaded
/// decoder only ever offers it per whole block.
///
/// The two implementations are the two widths the containers around LZMA use,
/// and each width has exactly one algorithm here: `u32` is CRC-32/ISO-HDLC (the
/// 7z and xz CRC-32) and `u64` is CRC-64/XZ. There is no way to ask for a
/// different polynomial, because there is no other one to ask for.
pub trait Foldable: Copy + Eq {
    /// The checksum of an empty range, which is the identity for [`Self::fold`].
    const EMPTY: Self;

    /// The checksum of `a`'s bytes followed by `b`'s, where `b` covered
    /// `len_b` bytes.
    #[must_use]
    fn fold(a: Self, b: Self, len_b: u64) -> Self;
}

/// CRC-32/ISO-HDLC of `a`'s bytes followed by `b`'s.
///
/// `len_b` is the number of bytes `b` was computed over. `crc_a` may be the
/// checksum of any prefix, including the empty one (`0`).
#[must_use]
pub fn crc32_combine(crc_a: u32, crc_b: u32, len_b: u64) -> u32 {
    crc_fast::checksum_combine(
        CrcAlgorithm::Crc32IsoHdlc,
        u64::from(crc_a),
        u64::from(crc_b),
        len_b,
    ) as u32
}

/// CRC-64/XZ of `a`'s bytes followed by `b`'s.
///
/// `len_b` is the number of bytes `b` was computed over.
#[must_use]
pub fn crc64_xz_combine(crc_a: u64, crc_b: u64, len_b: u64) -> u64 {
    crc_fast::checksum_combine(CrcAlgorithm::Crc64Xz, crc_a, crc_b, len_b)
}

impl Foldable for u32 {
    const EMPTY: Self = 0;

    fn fold(a: Self, b: Self, len_b: u64) -> Self {
        crc32_combine(a, b, len_b)
    }
}

impl Foldable for u64 {
    const EMPTY: Self = 0;

    fn fold(a: Self, b: Self, len_b: u64) -> Self {
        crc64_xz_combine(a, b, len_b)
    }
}

/// Collects the checksums of pieces of a stream, in any order, and folds them
/// into the checksum of any contiguous range the pieces tile exactly.
///
/// This is the consumer half of worker-side checksumming. A decoder that hands
/// out `(offset, len, checksum)` for the pieces it produced - possibly out of
/// order, certainly on several threads - lets its caller ask for the checksum
/// of a file spanning several of them without buffering the bytes.
///
/// Pieces are kept as pushed and folded only on a query, deliberately. A CRC
/// cannot be un-folded: if adjacent pieces were combined eagerly, a later
/// question about a range ending inside the combined run would be unanswerable.
/// So a range can be answered exactly when the pushed pieces tile it, with a
/// piece boundary at each end - which is what happens when the split points
/// given to the decoder are the boundaries the consumer will ask about.
///
/// Pushing overlapping pieces is a caller error; the last one at an offset
/// wins and the folded answers are then meaningless.
#[derive(Debug, Clone)]
pub struct CrcFolder<W: Foldable> {
    pieces: alloc::collections::BTreeMap<u64, (u64, W)>,
}

impl<W: Foldable> Default for CrcFolder<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<W: Foldable> CrcFolder<W> {
    /// An empty folder.
    #[must_use]
    pub fn new() -> Self {
        CrcFolder {
            pieces: alloc::collections::BTreeMap::new(),
        }
    }

    /// Adds the checksum of the `len` bytes at `offset`. Zero-length pieces
    /// are ignored.
    pub fn push(&mut self, offset: u64, len: u64, checksum: W) {
        if len != 0 {
            self.pieces.insert(offset, (len, checksum));
        }
    }

    /// The checksum of `len` bytes at `offset`, or `None` if the pieces held
    /// do not tile that range exactly.
    ///
    /// An empty range is [`Foldable::EMPTY`] whether or not anything has been
    /// pushed.
    #[must_use]
    pub fn range(&self, offset: u64, len: u64) -> Option<W> {
        if len == 0 {
            return Some(W::EMPTY);
        }
        let end = offset.checked_add(len)?;
        let mut pos = offset;
        // Folded from the first piece and not from the empty checksum:
        // combining a piece with nothing gives the piece back, and costs a
        // whole fold to do it.
        let mut acc = None;
        for (&s, &(l, c)) in self.pieces.range(offset..end) {
            if s != pos {
                return None;
            }
            acc = Some(match acc {
                None => c,
                Some(a) => W::fold(a, c, l),
            });
            pos = s.checked_add(l)?;
            if pos >= end {
                break;
            }
        }
        if pos == end { acc } else { None }
    }

    /// How many pieces are held.
    #[must_use]
    pub fn pieces(&self) -> usize {
        self.pieces.len()
    }

    /// Forgets everything pushed so far.
    pub fn clear(&mut self) {
        self.pieces.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check values both algorithms publish for the nine ASCII digits,
    /// plus the empty string, which is where an off-by-one in the initial or
    /// final xor shows up.
    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b"a"), 0xe8b7_be43);
        assert_eq!(crc64_xz(b""), 0x0000_0000_0000_0000);
        assert_eq!(crc64_xz(b"123456789"), 0x995d_c9bb_df19_39fa);
    }

    /// Splitting the stream must not change the answer; this is the property
    /// a container reader relies on when it checksums as it decodes.
    #[test]
    fn streaming_matches_one_shot() {
        let data: alloc::vec::Vec<u8> = (0u32..4096).map(|i| (i * 37 + 11) as u8).collect();
        for chunk in [1usize, 3, 64, 1000] {
            let mut a = Crc32::new();
            let mut b = Crc64Xz::new();
            for part in data.chunks(chunk) {
                a.update(part);
                b.update(part);
            }
            assert_eq!(a.finalize(), crc32(&data), "crc32, chunk {chunk}");
            assert_eq!(b.finalize(), crc64_xz(&data), "crc64, chunk {chunk}");
        }
    }
    /// Folding two adjacent pieces must give the one-shot checksum of the
    /// whole, at both widths. This is the property the multi-threaded decoder
    /// sells: a worker checksums only what it produced.
    #[test]
    fn combine_matches_one_shot() {
        let data: alloc::vec::Vec<u8> = (0u32..8192).map(|i| (i * 31 + 7) as u8).collect();
        for cut in [0usize, 1, 2, 17, 4096, 8191, 8192] {
            let (a, b) = data.split_at(cut);
            assert_eq!(
                crc32_combine(crc32(a), crc32(b), b.len() as u64),
                crc32(&data),
                "crc32, cut {cut}"
            );
            assert_eq!(
                crc64_xz_combine(crc64_xz(a), crc64_xz(b), b.len() as u64),
                crc64_xz(&data),
                "crc64, cut {cut}"
            );
        }
    }

    /// The empty checksum is the identity, which is what lets a folder start
    /// from nothing.
    #[test]
    fn empty_is_the_identity() {
        let data = b"the quick brown fox";
        assert_eq!(
            crc32_combine(u32::EMPTY, crc32(data), data.len() as u64),
            crc32(data)
        );
        assert_eq!(crc32_combine(crc32(data), u32::EMPTY, 0), crc32(data));
        assert_eq!(
            crc64_xz_combine(u64::EMPTY, crc64_xz(data), data.len() as u64),
            crc64_xz(data)
        );
        assert_eq!(
            crc64_xz_combine(crc64_xz(data), u64::EMPTY, 0),
            crc64_xz(data)
        );
    }

    /// Pieces pushed in any order fold to the one-shot checksum of every range
    /// they tile, and a range they do not tile is refused rather than guessed.
    #[test]
    fn folder_answers_tiled_ranges_only() {
        let data: alloc::vec::Vec<u8> = (0u32..1000).map(|i| (i * 13 + 5) as u8).collect();
        let bounds = [0usize, 1, 100, 101, 500, 999, 1000];

        let mut f32 = CrcFolder::<u32>::new();
        let mut f64 = CrcFolder::<u64>::new();
        // Pushed back to front, to prove order does not matter.
        for w in bounds.windows(2).rev() {
            let (a, b) = (w[0], w[1]);
            f32.push(a as u64, (b - a) as u64, crc32(&data[a..b]));
            f64.push(a as u64, (b - a) as u64, crc64_xz(&data[a..b]));
        }

        for (i, &a) in bounds.iter().enumerate() {
            for &b in &bounds[i..] {
                let len = (b - a) as u64;
                assert_eq!(
                    f32.range(a as u64, len),
                    Some(crc32(&data[a..b])),
                    "crc32 range {a}..{b}"
                );
                assert_eq!(
                    f64.range(a as u64, len),
                    Some(crc64_xz(&data[a..b])),
                    "crc64 range {a}..{b}"
                );
            }
        }

        // 50 is not a boundary, so neither end of a range at it can be folded.
        assert_eq!(f32.range(50, 50), None);
        assert_eq!(f32.range(0, 50), None);
        // Nor can a range run off the end of what has been pushed.
        assert_eq!(f32.range(0, 1001), None);

        // A hole in the middle is refused even though both sides are present.
        let mut holed = CrcFolder::<u32>::new();
        holed.push(0, 100, crc32(&data[..100]));
        holed.push(500, 100, crc32(&data[500..600]));
        assert_eq!(holed.range(0, 600), None);
        assert_eq!(holed.range(0, 100), Some(crc32(&data[..100])));
        assert_eq!(holed.pieces(), 2);
    }

    /// The delegating seam this module compiles on wasm, driven natively.
    ///
    /// `crc32_host` / `crc64_xz_host` are the two functions a `crc-host` wasm
    /// build folds every byte through, and they resolve to the embedder's real
    /// hooks on any target - `fn` pointers link everywhere - so installing the
    /// reference hook set lets the whole delegation path be proven here, with
    /// no wasm runtime in sight: registry lookup, seeded resume, and the chunk
    /// chaining the running-integer `Crc32` / `Crc64Xz` depend on.
    ///
    /// Chunking is exhaustive-ish rather than illustrative: every length gets
    /// the all-1-byte split (the most boundaries a stream can have), a
    /// pseudo-random split, and the whole buffer in one call.
    #[cfg(all(
        feature = "crc-host",
        any(feature = "crypto", feature = "native-crypto")
    ))]
    #[test]
    fn host_seam_chunk_chaining_matches_one_shot() {
        crate::hooks::install_reference_hooks_for_test();

        // A deterministic xorshift64* split generator - reproducible, and no
        // dependency. It picks split sizes only; the data is the same
        // arithmetic fill the tests above use.
        let mut state: u64 = 0x5EED_C0DE_1234_9001;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };

        for &len in &[0usize, 1, 2, 17, 64, 255, 256, 4096, 4097, 65_537] {
            let data: alloc::vec::Vec<u8> =
                (0..len).map(|i| (i.wrapping_mul(37) + 11) as u8).collect();

            let mut splits = alloc::vec::Vec::new();
            let mut remaining = len;
            while remaining > 0 {
                let take = 1 + (next() as usize) % remaining.min(4096);
                splits.push(take);
                remaining -= take;
            }
            let all_1: alloc::vec::Vec<usize> = alloc::vec![1usize; len];
            let whole = if len == 0 {
                alloc::vec::Vec::new()
            } else {
                alloc::vec![len]
            };

            for (label, sizes) in [("all-1", &all_1), ("random", &splits), ("whole", &whole)] {
                let (mut c32, mut c64) = (0u32, 0u64);
                let (mut s32, mut s64) = (Crc32::new(), Crc64Xz::new());
                let mut offset = 0;
                for &size in sizes {
                    let part = &data[offset..offset + size];
                    c32 = crc32_host(c32, part);
                    c64 = crc64_xz_host(c64, part);
                    s32.update(part);
                    s64.update(part);
                    offset += size;
                }
                assert_eq!(offset, len, "splits must cover the buffer");
                assert_eq!(c32, crc32(&data), "crc32 hook chain, len {len}, {label}");
                assert_eq!(c64, crc64_xz(&data), "crc64 hook chain, len {len}, {label}");
                // The public seam must agree with the one-shot too, whichever
                // backend this build compiled it as.
                assert_eq!(s32.finalize(), crc32(&data), "Crc32, len {len}, {label}");
                assert_eq!(
                    s64.finalize(),
                    crc64_xz(&data),
                    "Crc64Xz, len {len}, {label}"
                );
            }
        }
    }

    /// A stand-in checksum that carries the number of folds that made it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Counted {
        bytes: u64,
        folds: u32,
    }

    impl Foldable for Counted {
        const EMPTY: Self = Counted { bytes: 0, folds: 0 };

        fn fold(a: Self, b: Self, len_b: u64) -> Self {
            Counted {
                bytes: a.bytes + len_b,
                folds: a.folds + b.folds + 1,
            }
        }
    }

    /// A range is folded from its first piece, so a range that is one piece
    /// costs no fold and one of `n` pieces costs `n - 1`. A file that is
    /// exactly one piece used to pay for a fold to combine it with nothing.
    #[test]
    fn folder_folds_a_range_from_its_first_piece() {
        let mut f = CrcFolder::<Counted>::new();
        for i in 0..4u64 {
            let piece = Counted {
                bytes: 10,
                folds: 0,
            };
            f.push(i * 10, 10, piece);
        }
        for pieces in 1..=4u64 {
            for first in 0..=(4 - pieces) {
                let whole = Counted {
                    bytes: pieces * 10,
                    folds: pieces as u32 - 1,
                };
                assert_eq!(
                    f.range(first * 10, pieces * 10),
                    Some(whole),
                    "{pieces} piece(s) from piece {first}"
                );
            }
        }
    }

    /// An empty range needs nothing pushed at all.
    #[test]
    fn folder_answers_the_empty_range() {
        let f = CrcFolder::<u32>::new();
        assert_eq!(f.range(0, 0), Some(0));
        assert_eq!(f.range(12345, 0), Some(0));
    }
}
