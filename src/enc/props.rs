//! Encoder properties.
//!
//! C: `CLzmaEncProps`, `LzmaEncProps_Init`, `LzmaEncProps_Normalize` and
//! `LzmaEnc_WriteProperties` in `C/LzmaEnc.c` / `C/LzmaEnc.h`.
//!
//! The C's `-1` "not set" sentinels are kept, because `LzmaEncProps_Normalize`
//! is written in terms of them and the defaults it derives are part of what a
//! parity test compares. Callers never write `-1`: [`LzmaEncProps::new`] sets
//! every field to it and the setters take real values.
//!
//! One deliberate pin. `LzmaEncProps_Normalize`'s default dictionary size and
//! `kNumLogBits` are written in terms of `sizeof(size_t)`, so the C encodes a
//! file differently on a 32-bit host. This port always uses the 64-bit
//! numbers, so that one input and one setting give one output everywhere.

use crate::enc::consts::*;
use crate::enc::lz_find::MatchFinderKind;
use crate::error::Error;

/// C: `CLzmaEncProps`. The affinity fields are not carried - nothing in this
/// port pins a thread - but `numThreads` is: it is what turns on the threaded
/// match finder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LzmaEncProps {
    pub(crate) level: i32,
    pub(crate) dict_size: u32,
    pub(crate) lc: i32,
    pub(crate) lp: i32,
    pub(crate) pb: i32,
    pub(crate) algo: i32,
    pub(crate) fb: i32,
    pub(crate) bt_mode: i32,
    pub(crate) num_hash_bytes: i32,
    pub(crate) num_hash_out_bits: u32,
    pub(crate) mc: u32,
    pub(crate) write_end_mark: bool,
    pub(crate) reduce_size: u64,
    pub(crate) num_threads: i32,
}

impl Default for LzmaEncProps {
    fn default() -> Self {
        Self::new()
    }
}

impl LzmaEncProps {
    /// C: `LzmaEncProps_Init`.
    #[must_use]
    pub const fn new() -> Self {
        LzmaEncProps {
            level: 5,
            dict_size: 0,
            lc: -1,
            lp: -1,
            pb: -1,
            algo: -1,
            fb: -1,
            bt_mode: -1,
            num_hash_bytes: -1,
            num_hash_out_bits: 0,
            mc: 0,
            write_end_mark: false,
            reduce_size: u64::MAX,
            num_threads: -1,
        }
    }

    /// The settings `xz -N` uses, rather than the SDK's level `N`.
    ///
    /// [`LzmaEncProps::with_level`] is 7-Zip's numbering, resolved by
    /// `LzmaEncProps_Normalize`; liblzma numbers its presets differently, and
    /// the two disagree most at the fast end — the SDK's level 1 has a
    /// 256 KiB dictionary where `xz -1` has 1 MiB. This builds `xz`'s preset
    /// out of this encoder's own settings, so a stream written with it is
    /// directly comparable with what `xz -N` (or `--lzma2=preset=N`) writes:
    /// the same dictionary, `lc`/`lp`/`pb`, parser mode, match finder, nice
    /// length (fast bytes here) and search depth (match cycles here). With
    /// `extreme` it is `xz -Ne`.
    ///
    /// C: `lzma_lzma_preset` in XZ Utils' `liblzma/lzma/lzma_encoder_presets.c`,
    /// with the depth `lz_encoder.c` derives when the preset leaves it at zero.
    /// The one setting with no counterpart is preset 0's three-byte hash
    /// chain: `LzFind.c` has no `HC3`, so preset 0 gets the four-byte one.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] for a preset above 9, which liblzma refuses too.
    pub const fn xz_preset(preset: u32, extreme: bool) -> Result<Self, Error> {
        const DICT_POW2: [u8; 10] = [18, 20, 21, 22, 22, 23, 23, 24, 25, 26];
        const FAST_DEPTH: [u32; 4] = [4, 8, 24, 48];
        if preset > 9 {
            return Err(Error::Param);
        }
        let level = preset as usize;
        let (fast, kind, fb, mc) = if extreme {
            if preset == 3 || preset == 5 {
                (false, MatchFinderKind::Bt4, 192, 16 + 192 / 2)
            } else {
                (false, MatchFinderKind::Bt4, 273, 512)
            }
        } else if preset <= 3 {
            let fb = if preset <= 1 { 128 } else { 273 };
            (true, MatchFinderKind::Hc4, fb, FAST_DEPTH[level])
        } else {
            let fb = match preset {
                4 => 16,
                5 => 32,
                _ => 64,
            };
            // `lz_encoder.c`: a binary tree's depth defaults to
            // `16 + nice_len / 2`.
            (false, MatchFinderKind::Bt4, fb, 16 + fb / 2)
        };
        Ok(LzmaEncProps::new()
            .with_level(preset)
            .with_dict_size(1u32 << DICT_POW2[level])
            .with_lclppb(3, 0, 2)
            .with_fast_mode(fast)
            .with_match_finder(kind)
            .with_fast_bytes(fb)
            .with_match_cycles(mc))
    }

    /// How many threads one LZMA coder may use, which is 1 or 2: the second
    /// one is the threaded match finder, and it only applies in binary-tree
    /// mode outside fast mode. C: `props.numThreads`.
    ///
    /// The SDK, built without `Z7_ST`, defaults this to 2 whenever the match
    /// finder is a binary tree. This port defaults it to 1 - the `Z7_ST`
    /// value - so that output does not change under callers who never asked
    /// for a thread. See `docs/encoder.md`.
    #[must_use]
    pub const fn with_num_threads(mut self, threads: u32) -> Self {
        self.num_threads = threads as i32;
        self
    }

    /// Compression level, 0 to 9. C: `props.level`.
    #[must_use]
    pub const fn with_level(mut self, level: u32) -> Self {
        self.level = level as i32;
        self
    }

    /// Dictionary size in bytes. C: `props.dictSize`.
    #[must_use]
    pub const fn with_dict_size(mut self, dict_size: u32) -> Self {
        self.dict_size = dict_size;
        self
    }

    /// Literal context, literal position and position bits.
    /// C: `props.lc`, `props.lp`, `props.pb`.
    #[must_use]
    pub const fn with_lclppb(mut self, lc: u8, lp: u8, pb: u8) -> Self {
        self.lc = lc as i32;
        self.lp = lp as i32;
        self.pb = pb as i32;
        self
    }

    /// Number of fast bytes, 5 to 273. C: `props.fb`.
    #[must_use]
    pub const fn with_fast_bytes(mut self, fb: u32) -> Self {
        self.fb = fb as i32;
        self
    }

    /// Match finder cycles. C: `props.mc`, the match finder's `cutValue`.
    #[must_use]
    pub const fn with_match_cycles(mut self, mc: u32) -> Self {
        self.mc = mc;
        self
    }

    /// Which match finder to use. C: `props.btMode` and `props.numHashBytes`.
    #[must_use]
    pub const fn with_match_finder(mut self, kind: MatchFinderKind) -> Self {
        self.bt_mode = kind.bt_mode() as i32;
        self.num_hash_bytes = kind.num_hash_bytes() as i32;
        self
    }

    /// The optimal parser (`false`) or the fast one (`true`).
    /// C: `props.algo == 0`.
    #[must_use]
    pub const fn with_fast_mode(mut self, fast: bool) -> Self {
        self.algo = !fast as i32;
        self
    }

    /// Whether to write the end-of-payload marker.
    /// C: `props.writeEndMark`.
    #[must_use]
    pub const fn with_end_mark(mut self, write_end_mark: bool) -> Self {
        self.write_end_mark = write_end_mark;
        self
    }

    /// The size of the data about to be encoded, when it is known.
    ///
    /// C: `props.reduceSize`. It shrinks the dictionary to fit and, through
    /// `LzmaEnc_SetDataSize`, sizes the match finder's hash table — so it
    /// changes the bytes the encoder produces, not only what it allocates.
    #[must_use]
    pub const fn with_reduce_size(mut self, reduce_size: u64) -> Self {
        self.reduce_size = reduce_size;
        self
    }

    /// C: `LzmaEncProps_Normalize`.
    pub(crate) fn normalize(&mut self) {
        let mut level = self.level;
        if level < 0 {
            level = 5;
        }
        self.level = level;

        if self.dict_size == 0 {
            // C: the `sizeof(size_t)` terms, pinned to the 64-bit host.
            self.dict_size = if level as u32 <= 4 {
                1u32 << (level * 2 + 16)
            } else if level as u32 <= 8 {
                1u32 << (level + 20)
            } else {
                1u32 << 28
            };
        }

        if u64::from(self.dict_size) > self.reduce_size {
            let mut v = self.reduce_size as u32;
            let k_reduce_min = 1u32 << 12;
            if v < k_reduce_min {
                v = k_reduce_min;
            }
            if self.dict_size > v {
                self.dict_size = v;
            }
        }

        if self.lc < 0 {
            self.lc = 3;
        }
        if self.lp < 0 {
            self.lp = 0;
        }
        if self.pb < 0 {
            self.pb = 2;
        }
        if self.algo < 0 {
            self.algo = i32::from(level >= 5);
        }
        if self.fb < 0 {
            self.fb = if level < 7 { 32 } else { 64 };
        }
        if self.bt_mode < 0 {
            self.bt_mode = i32::from(self.algo != 0);
        }
        if self.num_hash_bytes < 0 {
            self.num_hash_bytes = if self.bt_mode != 0 { 4 } else { 5 };
        }
        if self.mc == 0 {
            self.mc = (16 + (self.fb as u32 >> 1)) >> u32::from(self.bt_mode == 0);
        }
        if self.num_threads < 0 {
            // C: `((p->btMode && p->algo) ? 2 : 1)` in a threaded build. This
            // port keeps the `Z7_ST` answer as its default; see
            // `LzmaEncProps::with_num_threads`.
            self.num_threads = 1;
        }
    }

    /// This setting with every `-1` resolved, as `LzmaEncProps_Normalize`
    /// leaves it. The reference encoder takes these numbers on its command
    /// line, so the parity tests need to see them.
    #[must_use]
    pub fn normalized(&self) -> NormalizedProps {
        let mut p = *self;
        p.normalize();
        NormalizedProps {
            level: p.level as u32,
            dict_size: p.dict_size,
            lc: p.lc as u32,
            lp: p.lp as u32,
            pb: p.pb as u32,
            fb: p.fb as u32,
            bt_mode: p.bt_mode as u32,
            num_hash_bytes: p.num_hash_bytes as u32,
            mc: p.mc,
            num_threads: p.num_threads as u32,
        }
    }

    /// C: the `lc + lp > LZMA2_LCLP_MAX` check in `Lzma2Enc_SetProps`.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] when the sum is above 4.
    pub(crate) fn check_lclp_for_lzma2(&self) -> Result<(), Error> {
        let mut normalized = *self;
        normalized.normalize();
        if normalized.lc + normalized.lp > LZMA2_LCLP_MAX {
            return Err(Error::Param);
        }
        Ok(())
    }

    /// The dictionary size this setting ends up with.
    ///
    /// C: `LzmaEncProps_GetDictSize`.
    #[must_use]
    pub fn dict_size(&self) -> u32 {
        let mut props = *self;
        props.normalize();
        props.dict_size
    }
}

/// An [`LzmaEncProps`] with every default resolved.
///
/// C: a `CLzmaEncProps` after `LzmaEncProps_Normalize`, minus the fields this
/// port does not carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct NormalizedProps {
    /// C: `level`.
    pub level: u32,
    /// C: `dictSize`.
    pub dict_size: u32,
    /// C: `lc`.
    pub lc: u32,
    /// C: `lp`.
    pub lp: u32,
    /// C: `pb`.
    pub pb: u32,
    /// C: `fb`, the number of fast bytes.
    pub fb: u32,
    /// C: `btMode`; 1 for a binary tree, 0 for a hash chain.
    pub bt_mode: u32,
    /// C: `numHashBytes`.
    pub num_hash_bytes: u32,
    /// C: `mc`, the match finder's cut value.
    pub mc: u32,
    /// C: `numThreads`.
    pub num_threads: u32,
}

/// C: `LzmaEnc_WriteProperties`.
pub(crate) fn write_properties(lc: u32, lp: u32, pb: u32, dict_size: u32) -> [u8; LZMA_PROPS_SIZE] {
    let mut props = [0u8; LZMA_PROPS_SIZE];
    props[0] = ((pb * 5 + lp) * 9 + lc) as u8;

    // C: "we write aligned dictionary value to properties for lzma decoder".
    let v = if dict_size >= (1 << 21) {
        let k_dict_mask = (1u32 << 20) - 1;
        let v = dict_size.wrapping_add(k_dict_mask) & !k_dict_mask;
        if v < dict_size { dict_size } else { v }
    } else {
        let mut i = 11 * 2u32;
        let mut v;
        loop {
            v = (2 + (i & 1)) << (i >> 1);
            i += 1;
            if v >= dict_size {
                break;
            }
        }
        v
    };
    props[1..5].copy_from_slice(&v.to_le_bytes());
    props
}

/// C: the parameter checks at the head of `LzmaEnc_SetProps`.
pub(crate) fn check(props: &LzmaEncProps) -> Result<(), Error> {
    if props.lc as u32 > LZMA_LC_MAX
        || props.lp as u32 > LZMA_LP_MAX
        || props.pb as u32 > LZMA_PB_MAX
    {
        return Err(Error::Param);
    }
    if u64::from(props.dict_size) > (1u64 << K_DIC_LOG_SIZE_MAX_COMPRESS) {
        return Err(Error::Param);
    }
    Ok(())
}
