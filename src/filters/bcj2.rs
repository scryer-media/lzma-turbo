//! BCJ2, the four-stream branch converter for x86 code, both directions.
//!
//! C: `C/Bcj2.c` (`Bcj2Dec_Init`, `Bcj2Dec_Decode`) and `C/Bcj2Enc.c`
//! (`Bcj2Enc_Init`, `Bcj2Enc_Encode`, `Bcj2Enc_Encode_2`), public domain.
//!
//! BCJ2 is not one of the in-place converters in [`bcj`](super::bcj). It
//! splits its input into four streams:
//!
//! - [`STREAM_MAIN`], the bytes that were not part of a converted branch;
//! - [`STREAM_CALL`], the absolute targets of converted `E8` calls, four
//!   big-endian bytes each;
//! - [`STREAM_JUMP`], the same for `E9` jumps and the `0F 8x` conditional
//!   jumps;
//! - [`STREAM_RC`], a range-coded bit per branch opcode saying whether its
//!   offset was converted.
//!
//! A 7z folder carries the four as separate coder outputs, which is why the
//! core of this module is slice-driven rather than reader-driven: the caller
//! hands over whatever it has of each stream and is told how much was taken.
//! [`Bcj2Dec::decode`] and [`Bcj2Enc::encode`] can be called again with more
//! of any stream, and the result is the same as one call with all of it.
//!
//! Two shapes of the format are worth knowing about, both from `Bcj2.h`:
//!
//! - if the last bytes of a stream are a marker (`E8`, `E9` or `0F 8x`), the
//!   range-coded stream still carries a symbol for it, even though there is
//!   no offset left to convert;
//! - one overlap is legal: when the last byte of a converted instruction is
//!   `0F` and the next byte is `8x`, that pair is itself a marker. The
//!   encoder's default relative limit keeps it from producing such an overlap,
//!   but the decoder accepts one.
//!
//! The `CALL` and `JUMP` streams are read and written four bytes at a time, so
//! the slices handed to a call must have a length that is a multiple of four;
//! the C says the same in `Bcj2.h`. A slice that is not is refused with
//! [`Bcj2Error::UnalignedStream`] rather than read past its end.

use alloc::vec;
use alloc::vec::Vec;

/// The number of streams a BCJ2 conversion has. C: `BCJ2_NUM_STREAMS`.
pub const NUM_STREAMS: usize = 4;

/// The bytes outside converted branches. C: `BCJ2_STREAM_MAIN`.
pub const STREAM_MAIN: usize = 0;
/// The absolute targets of converted `E8` calls. C: `BCJ2_STREAM_CALL`.
pub const STREAM_CALL: usize = 1;
/// The absolute targets of converted jumps. C: `BCJ2_STREAM_JUMP`.
pub const STREAM_JUMP: usize = 2;
/// The range-coded conversion flags. C: `BCJ2_STREAM_RC`.
pub const STREAM_RC: usize = 3;

/// C: `kTopValue`.
const TOP_VALUE: u32 = 1 << 24;
/// C: `kNumBitModelTotalBits`.
const NUM_BIT_MODEL_TOTAL_BITS: u32 = 11;
/// C: `kBitModelTotal`.
const BIT_MODEL_TOTAL: u32 = 1 << NUM_BIT_MODEL_TOTAL_BITS;
/// C: `kNumMoveBits`.
const NUM_MOVE_BITS: u32 = 5;
/// C: `sizeof(probs) / sizeof(probs[0])`, which is `2 + 256`.
const NUM_PROBS: usize = 2 + 256;

/// The default relative-offset limit. C: `BCJ2_ENC_RELAT_LIMIT_DEFAULT`.
///
/// An offset further than this is left alone, which is also what keeps the
/// encoder from emitting the `0F 8x` overlap the decoder tolerates.
pub const RELAT_LIMIT_DEFAULT: u32 = 0x0F << 24;
/// The largest limit the encoder accepts. C: `BCJ2_ENC_RELAT_LIMIT_MAX`.
pub const RELAT_LIMIT_MAX: u32 = 1 << 31;

/// C: `BCJ2_ENC_FileSizeField_UNLIMITED`.
const FILE_SIZE_UNLIMITED: u64 = u64::MAX;

/// What a BCJ2 conversion can refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Bcj2Error {
    /// The range-coded stream does not start the way the format says it must:
    /// C returns `SZ_ERROR_DATA` from `Bcj2Dec_Decode` for the same five
    /// bytes.
    Data,
    /// A `CALL` or `JUMP` slice whose length is not a multiple of four. Those
    /// two streams are four bytes per branch, and `Bcj2.h` makes the multiple
    /// a requirement on the caller.
    UnalignedStream,
}

impl core::fmt::Display for Bcj2Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Bcj2Error::Data => f.write_str("malformed BCJ2 range-coded stream"),
            Bcj2Error::UnalignedStream => {
                f.write_str("BCJ2 call/jump stream length is not a multiple of four")
            }
        }
    }
}

impl core::error::Error for Bcj2Error {}

// ---------------------------------------------------------------------------
// Little helpers, as in `bcj`: the C's GetBe32a/SetBe32a/SetUi32 with the
// alignment Rust does not need told.
// ---------------------------------------------------------------------------

#[inline]
fn get_be32(d: &[u8]) -> u32 {
    u32::from_be_bytes([d[0], d[1], d[2], d[3]])
}

#[inline]
fn set_be32(d: &mut [u8], i: usize, v: u32) {
    d[i..i + 4].copy_from_slice(&v.to_be_bytes());
}

#[inline]
fn set_u32le(d: &mut [u8], i: usize, v: u32) {
    d[i..i + 4].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn get_u32le(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
}

/// C: `BCJ2_IS_32BIT_STREAM(s)`.
#[inline]
fn is_32bit_stream(state: usize) -> bool {
    state.wrapping_sub(STREAM_CALL) < 2
}

/// C: the branch-opcode test in `ONE_ITER`, `((b + (0x100 - 0xe8)) & 0xfe) == 0`
/// — `b` is `E8` or `E9`.
#[inline]
fn is_branch_byte(b: u32) -> bool {
    (b + (0x100 - 0xE8)) & 0xFE == 0
}

/// C: the second half of `ONE_ITER`, which recognises the two-byte `0F 8x`
/// marker in the rolling two-byte context `v`.
#[inline]
fn is_long_jump(v: u32) -> bool {
    v.wrapping_sub((0x0F << 24) + 0x80) & 0xFFFF_FFF0 == 0
}

/// C: the probability index, `((0 - c) & (Byte)(v >> 24)) + c + ((v >> 5) & 1)`
/// — `0` and `1` for the two shapes of jump, `2 + prev` for a call.
#[inline]
fn prob_index(v: u32) -> usize {
    let c = (v.wrapping_add(0x17) >> 6) & 1;
    (((0u32.wrapping_sub(c)) & ((v >> 24) & 0xFF))
        .wrapping_add(c)
        .wrapping_add((v >> 5) & 1)) as usize
}

/// C: `cj = (((v + 0x57) >> 6) & 1) + BCJ2_STREAM_CALL`.
#[inline]
fn branch_stream(v: u32) -> usize {
    ((v.wrapping_add(0x57) >> 6) & 1) as usize + STREAM_CALL
}

// ===========================================================================
// Decoder. C: Bcj2.c.
// ===========================================================================

/// C: `BCJ2_DEC_STATE_ORIG_0`.
const DEC_STATE_ORIG_0: usize = NUM_STREAMS;
/// C: `BCJ2_DEC_STATE_ORIG_3`.
const DEC_STATE_ORIG_3: usize = NUM_STREAMS + 3;
/// C: `BCJ2_DEC_STATE_ORIG`.
const DEC_STATE_ORIG: usize = NUM_STREAMS + 4;
/// C: `BCJ2_DEC_STATE_ERROR`.
const DEC_STATE_ERROR: usize = NUM_STREAMS + 5;

/// The four input slices and the output window of one [`Bcj2Dec::decode`]
/// call.
///
/// C: `CBcj2Dec::bufs`, `lims`, `dest` and `destLim`. Each input slice is
/// consumed from the front, so what is left in [`bufs`](Self::bufs) after a
/// call is what the next call must be given again; the output is written at
/// [`dest_pos`](Self::dest_pos), which the call advances.
#[derive(Debug)]
pub struct Bcj2DecStreams<'a> {
    /// The four input streams, indexed by [`STREAM_MAIN`] and friends. A call
    /// replaces each with what it did not take.
    pub bufs: [&'a [u8]; NUM_STREAMS],
    /// Where the converted bytes go.
    pub dest: &'a mut [u8],
    /// How much of `dest` has been written. A call advances it.
    pub dest_pos: usize,
}

impl<'a> Bcj2DecStreams<'a> {
    /// The four streams and an output window, with nothing yet written.
    #[must_use]
    pub fn new(
        main: &'a [u8],
        call: &'a [u8],
        jump: &'a [u8],
        rc: &'a [u8],
        dest: &'a mut [u8],
    ) -> Self {
        Bcj2DecStreams {
            bufs: [main, call, jump, rc],
            dest,
            dest_pos: 0,
        }
    }
}

/// The BCJ2 decoder: the state `Bcj2Dec_Decode` carries between calls.
///
/// C: `CBcj2Dec`, minus the buffer pointers, which are a
/// [`Bcj2DecStreams`] per call rather than fields.
#[derive(Debug, Clone)]
pub struct Bcj2Dec {
    /// C: `CBcj2Dec::state`.
    state: usize,
    /// C: `CBcj2Dec::ip`, the virtual address of the next output byte.
    ip: u32,
    /// C: `CBcj2Dec::temp`, up to four output bytes not yet written.
    temp: u32,
    range: u32,
    code: u32,
    probs: [u16; NUM_PROBS],
}

impl Default for Bcj2Dec {
    fn default() -> Self {
        Self::new()
    }
}

impl Bcj2Dec {
    /// A decoder starting at virtual address zero. C: `Bcj2Dec_Init`.
    #[must_use]
    pub fn new() -> Self {
        Bcj2Dec {
            state: STREAM_RC,
            ip: 0,
            temp: 0,
            range: 0,
            code: 0,
            probs: [(BIT_MODEL_TOTAL >> 1) as u16; NUM_PROBS],
        }
    }

    /// The same, at a given virtual address. C: the note on `Bcj2Dec_Init`,
    /// which leaves `ip` at zero and tells the caller to set it afterwards if
    /// the folder says otherwise.
    #[must_use]
    pub fn with_ip(ip: u32) -> Self {
        let mut p = Self::new();
        p.ip = ip;
        p
    }

    /// The virtual address of the next output byte. C: `CBcj2Dec::ip`.
    #[must_use]
    pub fn ip(&self) -> u32 {
        self.ip
    }

    /// Whether the decoder is waiting for more of the main stream rather than
    /// holding bytes it could not write. C:
    /// `Bcj2Dec_IsMaybeFinished_state_MAIN`.
    #[must_use]
    pub fn is_maybe_finished_state_main(&self) -> bool {
        self.state == STREAM_MAIN
    }

    /// Whether the range coder ended where a complete stream ends. C:
    /// `Bcj2Dec_IsMaybeFinished_code`.
    #[must_use]
    pub fn is_maybe_finished_code(&self) -> bool {
        self.code == 0
    }

    /// Both of the checks above. As the C says, this is an additional check
    /// and not a substitute for comparing the decoded size with the size the
    /// container recorded. C: `Bcj2Dec_IsMaybeFinished`.
    #[must_use]
    pub fn is_maybe_finished(&self) -> bool {
        self.is_maybe_finished_state_main() && self.is_maybe_finished_code()
    }

    /// Converts as much as the given streams allow.
    ///
    /// Returns when the output window is full, when a stream the decoder needs
    /// has run out, or when everything on offer has been converted; in every
    /// case what was consumed is gone from `s.bufs` and what was produced is
    /// counted by `s.dest_pos`. Call again with more of each stream.
    ///
    /// C: `Bcj2Dec_Decode`.
    ///
    /// # Errors
    ///
    /// [`Bcj2Error::Data`] if the first five bytes of the range-coded stream
    /// are not a valid range-coder priming (C: `SZ_ERROR_DATA`), and
    /// [`Bcj2Error::UnalignedStream`] if the call or jump slice is not a
    /// multiple of four bytes long.
    #[allow(clippy::too_many_lines)]
    pub fn decode(&mut self, s: &mut Bcj2DecStreams<'_>) -> Result<(), Bcj2Error> {
        if !s.bufs[STREAM_CALL].len().is_multiple_of(4)
            || !s.bufs[STREAM_JUMP].len().is_multiple_of(4)
        {
            return Err(Bcj2Error::UnalignedStream);
        }

        let mut v = self.temp;

        // C: the priming of the range coder from the first five bytes of the
        // RC stream, which uses `range` as the counter so that a call that
        // runs out part way through can be resumed. `state` is left at ERROR
        // on the way through: the two tests below both fail for it, and the
        // main loop sets a real state before returning.
        if self.range <= 5 {
            let mut code = self.code;
            self.state = DEC_STATE_ERROR;
            while self.range != 5 {
                if self.range == 1 && code != 0 {
                    return Err(Bcj2Error::Data);
                }
                let Some((&b, rest)) = s.bufs[STREAM_RC].split_first() else {
                    self.state = STREAM_RC;
                    return Ok(());
                };
                s.bufs[STREAM_RC] = rest;
                code = (code << 8) | u32::from(b);
                self.code = code;
                self.range += 1;
            }
            if code == 0xFFFF_FFFF {
                return Err(Bcj2Error::Data);
            }
            self.range = 0xFFFF_FFFF;
        }

        {
            let mut state = self.state;
            // C: the 32-bit streams are checked here rather than in the main
            // loop — the previous call stopped for want of a branch target.
            if is_32bit_stream(state) {
                let cur = s.bufs[state];
                if cur.is_empty() {
                    return Ok(());
                }
                s.bufs[state] = &cur[4..];
                let ip = self.ip.wrapping_add(4);
                v = get_be32(cur).wrapping_sub(ip);
                self.ip = ip;
                state = DEC_STATE_ORIG_0;
            }
            // C: the bytes of a branch target the previous call could not fit
            // in `dest`.
            if state.wrapping_sub(DEC_STATE_ORIG_0) < 4 {
                loop {
                    if s.dest_pos == s.dest.len() {
                        self.state = state;
                        self.temp = v;
                        return Ok(());
                    }
                    s.dest[s.dest_pos] = v as u8;
                    s.dest_pos += 1;
                    state += 1;
                    if state == DEC_STATE_ORIG_3 + 1 {
                        break;
                    }
                    v >>= 8;
                }
            }
        }

        loop {
            // C: the range coder's normalization, which is the only place the
            // decoder reads the RC stream once it is primed.
            if self.range < TOP_VALUE {
                let Some((&b, rest)) = s.bufs[STREAM_RC].split_first() else {
                    self.state = STREAM_RC;
                    self.temp = v;
                    return Ok(());
                };
                s.bufs[STREAM_RC] = rest;
                self.range <<= 8;
                self.code = (self.code << 8) | u32::from(b);
            }

            {
                // C: the `ONE_ITER` loop, which copies the main stream to the
                // output until it meets a branch opcode. The C unrolls it four
                // at a time behind a length rounded down to a multiple of four
                // and then finishes byte at a time; both arms copy one byte,
                // test it, and stop on the same byte, and the amount consumed
                // from the main stream is taken from how far `dest` moved, so
                // one byte-at-a-time loop is the same function.
                let main = s.bufs[STREAM_MAIN];
                let mut copied = 0usize;
                let mut found = false;
                while copied != main.len() && s.dest_pos != s.dest.len() {
                    let b = u32::from(main[copied]);
                    s.dest[s.dest_pos] = b as u8;
                    s.dest_pos += 1;
                    copied += 1;
                    v = (v << 24) | b;
                    if is_branch_byte(b) || is_long_jump(v) {
                        found = true;
                        break;
                    }
                }
                s.bufs[STREAM_MAIN] = &main[copied..];
                self.ip = self.ip.wrapping_add(copied as u32);

                if !found {
                    // C: "state BCJ2_STREAM_MAIN has more priority than
                    // BCJ2_STATE_ORIG" — running out of input outranks
                    // running out of room.
                    self.state = if s.bufs[STREAM_MAIN].is_empty() {
                        STREAM_MAIN
                    } else {
                        DEC_STATE_ORIG
                    };
                    self.temp = v;
                    return Ok(());
                }

                // C: the one range-coded bit, saying whether this opcode's
                // offset was converted.
                let idx = prob_index(v);
                let ttt = u32::from(self.probs[idx]);
                let bound = (self.range >> NUM_BIT_MODEL_TOTAL_BITS) * ttt;
                if self.code < bound {
                    self.range = bound;
                    self.probs[idx] = (ttt + ((BIT_MODEL_TOTAL - ttt) >> NUM_MOVE_BITS)) as u16;
                    continue;
                }
                self.range -= bound;
                self.code -= bound;
                self.probs[idx] = (ttt - (ttt >> NUM_MOVE_BITS)) as u16;
            }

            {
                // C: the converted branch — an absolute target from the call
                // or jump stream, turned back into a relative offset.
                let cj = branch_stream(v);
                let cur = s.bufs[cj];
                if cur.is_empty() {
                    self.state = cj;
                    break;
                }
                v = get_be32(cur);
                s.bufs[cj] = &cur[4..];
                let ip = self.ip.wrapping_add(4);
                v = v.wrapping_sub(ip);
                self.ip = ip;

                let rem = s.dest.len() - s.dest_pos;
                if rem < 4 {
                    for _ in 0..rem {
                        s.dest[s.dest_pos] = v as u8;
                        s.dest_pos += 1;
                        v >>= 8;
                    }
                    self.temp = v;
                    self.state = DEC_STATE_ORIG_0 + rem;
                    break;
                }
                set_u32le(s.dest, s.dest_pos, v);
                v >>= 24;
                s.dest_pos += 4;
            }
        }

        // C: one last normalization, so that a stream that ended exactly here
        // leaves `code` at zero for `Bcj2Dec_IsMaybeFinished_code`.
        if self.range < TOP_VALUE
            && let Some((&b, rest)) = s.bufs[STREAM_RC].split_first()
        {
            s.bufs[STREAM_RC] = rest;
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(b);
        }
        Ok(())
    }
}

/// Converts four complete BCJ2 streams back into `orig_size` bytes in one
/// call.
///
/// The container knows the original size — a 7z folder records it — so this
/// is the shape a reader with all four sub-streams in memory wants. The
/// incremental [`Bcj2Dec::decode`] is underneath it.
///
/// # Errors
///
/// Whatever [`Bcj2Dec::decode`] returns, and [`Bcj2Error::Data`] if the four
/// streams do not in fact produce `orig_size` bytes.
pub fn decode_to_vec(
    main: &[u8],
    call: &[u8],
    jump: &[u8],
    rc: &[u8],
    orig_size: usize,
) -> Result<Vec<u8>, Bcj2Error> {
    let mut out = vec![0u8; orig_size];
    let mut dec = Bcj2Dec::new();
    let mut s = Bcj2DecStreams::new(main, call, jump, rc, &mut out);
    dec.decode(&mut s)?;
    if s.dest_pos != orig_size || !s.bufs[STREAM_MAIN].is_empty() {
        return Err(Bcj2Error::Data);
    }
    Ok(out)
}

// ===========================================================================
// Encoder. C: Bcj2Enc.c.
// ===========================================================================

/// C: `BCJ2_ENC_STATE_ORIG`.
const ENC_STATE_ORIG: usize = NUM_STREAMS;
/// C: `BCJ2_ENC_STATE_FINISHED`.
const ENC_STATE_FINISHED: usize = NUM_STREAMS + 1;

/// What the encoder should do when it reaches the end of the source it has.
///
/// C: `EBcj2Enc_FinishMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bcj2EncFinishMode {
    /// More input will follow, so keep up to four trailing bytes back rather
    /// than deciding about a marker that may continue.
    /// C: `BCJ2_ENC_FINISH_MODE_CONTINUE`.
    #[default]
    Continue,
    /// The block ends here: finish every branch it contains and flush the
    /// three plain streams, but do not flush the range coder, which spans
    /// blocks. C: `BCJ2_ENC_FINISH_MODE_END_BLOCK`.
    EndBlock,
    /// The stream ends here: flush the range coder too.
    /// C: `BCJ2_ENC_FINISH_MODE_END_STREAM`.
    EndStream,
}

/// The four output windows of one [`Bcj2Enc::encode`] call.
///
/// C: `CBcj2Enc::bufs` and `lims`. Each stream is written from
/// `pos[i]` onwards, and the call advances it.
#[derive(Debug)]
pub struct Bcj2EncOut<'a> {
    /// The four output windows, indexed by [`STREAM_MAIN`] and friends.
    pub bufs: [&'a mut [u8]; NUM_STREAMS],
    /// How much of each window has been written.
    pub pos: [usize; NUM_STREAMS],
}

impl<'a> Bcj2EncOut<'a> {
    /// Four empty output windows.
    #[must_use]
    pub fn new(
        main: &'a mut [u8],
        call: &'a mut [u8],
        jump: &'a mut [u8],
        rc: &'a mut [u8],
    ) -> Self {
        Bcj2EncOut {
            bufs: [main, call, jump, rc],
            pos: [0; NUM_STREAMS],
        }
    }

    #[inline]
    fn is_full(&self, i: usize) -> bool {
        self.pos[i] == self.bufs[i].len()
    }

    /// Whether a four-byte branch target still fits in stream `i`.
    ///
    /// C: `cur == p->lims[cj]`, which is the same test only because `Bcj2.h`
    /// requires the call and jump windows to be a multiple of four bytes long.
    /// Asking for the four bytes rather than for one keeps a window that is
    /// not — the encoder then stops one target earlier and the caller gives it
    /// more room, which changes nothing about the bytes produced.
    #[inline]
    fn has_target_room(&self, i: usize) -> bool {
        self.bufs[i].len() - self.pos[i] >= 4
    }
}

/// The BCJ2 encoder: the state `Bcj2Enc_Encode` carries between calls.
///
/// C: `CBcj2Enc`, minus the buffer pointers, which are a [`Bcj2EncOut`] and a
/// source slice per call rather than fields.
#[derive(Debug, Clone)]
pub struct Bcj2Enc {
    state: usize,
    finish_mode: Bcj2EncFinishMode,
    /// C: `CBcj2Enc::context`, the byte before the one being looked at.
    context: u8,
    flush_rem: u8,
    is_flush_state: bool,
    cache: u8,
    range: u32,
    low: u64,
    cache_size: u64,
    /// C: `ip64`, the virtual address of the next source byte, not counting
    /// what is held in `temp`.
    ip64: u64,
    file_ip64: u64,
    file_size64_minus1: u64,
    relat_limit: u32,
    temp_target: u32,
    temp_pos: usize,
    temp: [u8; 8],
    probs: [u16; NUM_PROBS],
}

impl Default for Bcj2Enc {
    fn default() -> Self {
        Self::new()
    }
}

impl Bcj2Enc {
    /// An encoder with the SDK's defaults: virtual address zero, no file-size
    /// limit and [`RELAT_LIMIT_DEFAULT`]. C: `Bcj2Enc_Init`.
    #[must_use]
    pub fn new() -> Self {
        Bcj2Enc {
            state: ENC_STATE_ORIG,
            finish_mode: Bcj2EncFinishMode::Continue,
            context: 0,
            flush_rem: 5,
            is_flush_state: false,
            cache: 0,
            range: 0xFFFF_FFFF,
            low: 0,
            cache_size: 1,
            ip64: 0,
            file_ip64: 0,
            file_size64_minus1: FILE_SIZE_UNLIMITED,
            relat_limit: RELAT_LIMIT_DEFAULT,
            temp_target: 0,
            temp_pos: 0,
            temp: [0; 8],
            probs: [(BIT_MODEL_TOTAL >> 1) as u16; NUM_PROBS],
        }
    }

    /// Sets what the encoder does at the end of the source it is given.
    /// C: `CBcj2Enc::finishMode`.
    pub fn set_finish_mode(&mut self, mode: Bcj2EncFinishMode) {
        self.finish_mode = mode;
    }

    /// Sets the virtual address of the next source byte, and of the start of
    /// the file the conversion limit is measured against.
    /// C: `CBcj2Enc::ip64` and `fileIp64`.
    pub fn set_ip(&mut self, ip: u64, file_ip: u64) {
        self.ip64 = ip;
        self.file_ip64 = file_ip;
    }

    /// Limits conversion to targets inside a file of this size: an offset
    /// whose absolute target lands outside is left alone. C:
    /// `Bcj2Enc_SET_FileSize`.
    pub fn set_file_size(&mut self, file_size: u64) {
        self.file_size64_minus1 = file_size.wrapping_sub(1);
    }

    /// Drops the file-size limit again.
    /// C: `BCJ2_ENC_FileSizeField_UNLIMITED`.
    pub fn clear_file_size(&mut self) {
        self.file_size64_minus1 = FILE_SIZE_UNLIMITED;
    }

    /// Sets how far a relative offset may reach and still be converted.
    /// Zero disables conversion altogether; the value is clamped to
    /// [`RELAT_LIMIT_MAX`], which is the C's documented ceiling.
    /// C: `CBcj2Enc::relatLimit`.
    pub fn set_relat_limit(&mut self, limit: u32) {
        self.relat_limit = limit.min(RELAT_LIMIT_MAX);
    }

    /// Which stream the last call stopped for, if it stopped for one of them:
    /// that window was full. C: `p->state < BCJ2_NUM_STREAMS`.
    #[must_use]
    pub fn full_stream(&self) -> Option<usize> {
        (self.state < NUM_STREAMS).then_some(self.state)
    }

    /// Whether the range coder has been flushed, which only happens under
    /// [`Bcj2EncFinishMode::EndStream`]. C: `Bcj2Enc_IsFinished`.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.flush_rem == 0
    }

    /// The number of source bytes the encoder is holding back for lookahead.
    /// C: `Bcj2Enc_Get_AvailInputSize_in_Temp`.
    #[must_use]
    pub fn held_input(&self) -> usize {
        self.temp_pos
    }

    /// The virtual address of the next source byte, not counting what is held
    /// back. C: `CBcj2Enc::ip64`.
    #[must_use]
    pub fn ip(&self) -> u64 {
        self.ip64
    }

    /// C: `Bcj2_RangeEnc_ShiftLow`. Returns true if it ran out of room in the
    /// range-coded stream, which is the caller's signal to stop.
    fn shift_low(&mut self, out: &mut Bcj2EncOut<'_>) -> bool {
        let low = self.low as u32;
        let high = (self.low >> 32) as u8;
        if low < 0xFF00_0000 || high != 0 {
            loop {
                if out.is_full(STREAM_RC) {
                    self.state = STREAM_RC;
                    return true;
                }
                let p = out.pos[STREAM_RC];
                out.bufs[STREAM_RC][p] = self.cache.wrapping_add(high);
                out.pos[STREAM_RC] = p + 1;
                self.cache = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = u64::from(low << 8);
        false
    }

    /// C: `Bcj2Enc_Encode_2`, the encoder proper. It may stop with up to four
    /// unprocessed source bytes in `CONTINUE` mode; [`encode`](Self::encode)
    /// is the wrapper that hides that behind `temp`.
    #[allow(clippy::too_many_lines)]
    fn encode_2(&mut self, out: &mut Bcj2EncOut<'_>, src_all: &[u8], src_pos: &mut usize) {
        if !self.is_flush_state {
            {
                // C: a previous call stopped with a branch target it could not
                // write; write it now.
                let state = self.state;
                if is_32bit_stream(state) {
                    if !out.has_target_room(state) {
                        return;
                    }
                    let p = out.pos[state];
                    set_be32(out.bufs[state], p, self.temp_target);
                    out.pos[state] = p + 4;
                }
            }
            self.state = ENC_STATE_ORIG;
            let mut src = *src_pos;
            let mut v = u32::from(self.context);

            loop {
                let mut ip: u64;
                if self.range < TOP_VALUE {
                    *src_pos = src;
                    self.context = v as u8;
                    if self.shift_low(out) {
                        return;
                    }
                    self.range <<= 8;
                    src = *src_pos;
                    v = u32::from(self.context);
                }
                {
                    // C: the `ONE_ITER` loop again, the copy from the source to
                    // the main stream that stops at a branch opcode. The C
                    // writes it twice per turn to keep the compiler from
                    // folding the two tests into one `setcc`; that is a
                    // code-generation trick, not a difference in what it does.
                    let dest_start = out.pos[STREAM_MAIN];
                    let rem_src = src_all.len() - src;
                    let rem = (out.bufs[STREAM_MAIN].len() - dest_start).min(rem_src);
                    let src_lim = src + rem;

                    if src != src_lim {
                        loop {
                            let b = u32::from(src_all[src]);
                            let p = out.pos[STREAM_MAIN];
                            out.bufs[STREAM_MAIN][p] = b as u8;
                            out.pos[STREAM_MAIN] = p + 1;
                            v = (v << 24) | b;
                            if is_branch_byte(b) || is_long_jump(v) {
                                break;
                            }
                            src += 1;
                            if src == src_lim {
                                break;
                            }
                        }
                    }

                    ip = self
                        .ip64
                        .wrapping_add((out.pos[STREAM_MAIN] - dest_start) as u64);
                    self.ip64 = ip;

                    if src == src_lim {
                        *src_pos = src;
                        self.context = v as u8;
                        if src != src_all.len() {
                            // The main stream filled before the source ran out.
                            self.state = STREAM_MAIN;
                            return;
                        }
                        if self.finish_mode != Bcj2EncFinishMode::EndStream {
                            return;
                        }
                        self.is_flush_state = true;
                        break;
                    }
                    src += 1;
                }

                // A marker: `v`'s top byte is the byte before it and its low
                // byte is the opcode.
                {
                    if src_all.len() - src >= 4 {
                        let relat = get_u32le(src_all, src);
                        ip = ip.wrapping_sub(self.file_ip64);
                        // C: the three conditions, in the C's order — the
                        // first rules out a marker that straddles a block
                        // boundary, the second keeps the absolute target
                        // inside the file, the third inside the relative
                        // limit.
                        if ip > u64::from((v.wrapping_add(0x20) >> 5) & 1)
                            && (ip as i64)
                                .wrapping_add(4)
                                .wrapping_add(i64::from(relat as i32))
                                as u64
                                <= self.file_size64_minus1
                            && (relat.wrapping_add(self.relat_limit) >> 1) < self.relat_limit
                        {
                            v |= CONV_FLAG;
                        }
                    } else if self.finish_mode == Bcj2EncFinishMode::Continue {
                        // Not enough source left to see the offset, and more
                        // is promised: give the marker byte back and ask for
                        // it again next time.
                        self.ip64 = self.ip64.wrapping_sub(1);
                        out.pos[STREAM_MAIN] -= 1;
                        src -= 1;
                        v >>= 24;
                        *src_pos = src;
                        self.context = v as u8;
                        return;
                    }

                    {
                        let idx = prob_index(v);
                        let ttt = u32::from(self.probs[idx]);
                        let bound = (self.range >> NUM_BIT_MODEL_TOTAL_BITS) * ttt;
                        if v & CONV_FLAG == 0 {
                            self.range = bound;
                            self.probs[idx] =
                                (ttt + ((BIT_MODEL_TOTAL - ttt) >> NUM_MOVE_BITS)) as u16;
                            continue;
                        }
                        self.low += u64::from(bound);
                        self.range -= bound;
                        self.probs[idx] = (ttt - (ttt >> NUM_MOVE_BITS)) as u16;
                    }

                    {
                        let cj = branch_stream(v);
                        ip = self.ip64;
                        v = get_u32le(src_all, src);
                        ip = ip.wrapping_add(4);
                        self.ip64 = ip;
                        src += 4;
                        let absol = (ip as u32).wrapping_add(v);
                        v >>= 24;
                        if !out.has_target_room(cj) {
                            self.state = cj;
                            self.temp_target = absol;
                            *src_pos = src;
                            self.context = v as u8;
                            return;
                        }
                        let p = out.pos[cj];
                        set_be32(out.bufs[cj], p, absol);
                        out.pos[cj] = p + 4;
                    }
                }
            }
        }

        // C: the range coder's five flush bytes.
        while self.flush_rem != 0 {
            if self.shift_low(out) {
                return;
            }
            self.flush_rem -= 1;
        }
        self.state = ENC_STATE_FINISHED;
    }

    /// Converts as much of `src` as the output windows allow, writing the four
    /// streams into `out`.
    ///
    /// `src_pos` says where in `src` to start and is advanced past everything
    /// the call took, including up to four bytes it kept back for lookahead
    /// (see [`held_input`](Self::held_input)). When the call returns with
    /// [`full_stream`](Self::full_stream) set, that window was full and the
    /// call should be made again with more room; otherwise it wants more
    /// source, or — under [`Bcj2EncFinishMode::EndStream`] — has finished.
    ///
    /// C: `Bcj2Enc_Encode`.
    pub fn encode(&mut self, out: &mut Bcj2EncOut<'_>, src: &[u8], src_pos: &mut usize) {
        if self.temp_pos != 0 {
            // C: the lookahead buffer. The held bytes are encoded first, one
            // byte of new source added at a time, so that no more of `src` is
            // touched than the encoder actually needs.
            let mut extra = 0usize;
            loop {
                let src_at = *src_pos;
                let finish_mode = self.finish_mode;
                if src_at != src.len() {
                    // There is more source after the held bytes, so the held
                    // bytes are not the end of anything.
                    self.finish_mode = Bcj2EncFinishMode::Continue;
                }
                // The C swaps `p->src` to `p->temp`; `temp` is copied out
                // first here because `encode_2` takes the encoder by mutable
                // reference and reads the source by shared reference.
                let temp = self.temp;
                let mut temp_pos = 0usize;
                self.encode_2(out, &temp[..self.temp_pos], &mut temp_pos);

                let num = temp_pos;
                let left = self.temp_pos - num;
                self.temp.copy_within(num..num + left, 0);
                self.temp_pos = left;
                *src_pos = src_at;
                self.finish_mode = finish_mode;

                if self.state != ENC_STATE_ORIG {
                    // C: the optional rollback — give back the bytes of `src`
                    // that were only ever copied into `temp` this call.
                    let extra = extra.min(left);
                    *src_pos = src_at - extra;
                    self.temp_pos = left - extra;
                    return;
                }
                if src_at == src.len() {
                    return;
                }
                if extra >= left {
                    // Everything in `temp` came from this call's `src`, so the
                    // rest can be encoded straight out of `src`.
                    *src_pos = src_at - left;
                    self.temp_pos = 0;
                    break;
                }
                self.temp[left] = src[src_at];
                self.temp_pos = left + 1;
                *src_pos = src_at + 1;
                extra += 1;
            }
        }

        self.encode_2(out, src, src_pos);

        if self.state == ENC_STATE_ORIG {
            // C: whatever the encoder could not decide about — at most four
            // bytes — goes into `temp` and the source counts as consumed.
            let rem = src.len() - *src_pos;
            if rem != 0 {
                self.temp[..rem].copy_from_slice(&src[*src_pos..]);
                *src_pos = src.len();
                self.temp_pos = rem;
            }
        }
    }
}

/// C: `CONV_FLAG`, the bit `Bcj2Enc_Encode_2` sets in its rolling context to
/// mean "this marker's offset is being converted".
const CONV_FLAG: u32 = 1 << 16;

/// The four streams a BCJ2 conversion produces.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Bcj2Streams {
    /// The bytes outside converted branches.
    pub main: Vec<u8>,
    /// Absolute targets of converted calls, big-endian, four bytes each.
    pub call: Vec<u8>,
    /// Absolute targets of converted jumps, likewise.
    pub jump: Vec<u8>,
    /// The range-coded conversion flags.
    pub rc: Vec<u8>,
}

/// Converts a whole buffer in one go, growing the four output streams as the
/// encoder needs them.
///
/// The incremental [`Bcj2Enc::encode`] is underneath it; this is the shape a
/// writer with the whole member in memory wants. `relat_limit` is
/// [`RELAT_LIMIT_DEFAULT`] and there is no file-size limit, which is what
/// `Bcj2Enc_Init` leaves behind.
#[must_use]
pub fn encode_to_streams(src: &[u8]) -> Bcj2Streams {
    let mut enc = Bcj2Enc::new();
    enc.set_finish_mode(Bcj2EncFinishMode::EndStream);
    encode_to_streams_with(&mut enc, src)
}

/// The same, with an encoder the caller has already configured — a relative
/// limit, a file size, a starting address. Its finish mode is set to
/// [`Bcj2EncFinishMode::EndStream`], because this converts a whole buffer.
pub fn encode_to_streams_with(enc: &mut Bcj2Enc, src: &[u8]) -> Bcj2Streams {
    enc.set_finish_mode(Bcj2EncFinishMode::EndStream);
    // Room for the common case in one go: the main stream is never longer than
    // the source, and a four-byte target costs five source bytes. The loop
    // below grows whatever still runs out, so none of this has to be a bound.
    let mut bufs: [Vec<u8>; NUM_STREAMS] = [
        vec![0u8; src.len() + 8],
        vec![0u8; src.len().div_ceil(5) * 4 + 16],
        vec![0u8; src.len().div_ceil(5) * 4 + 16],
        vec![0u8; src.len() / 4 + 64],
    ];
    let mut pos = [0usize; NUM_STREAMS];
    let mut src_pos = 0usize;

    loop {
        let [main, call, jump, rc] = &mut bufs;
        let mut out = Bcj2EncOut {
            bufs: [
                main.as_mut_slice(),
                call.as_mut_slice(),
                jump.as_mut_slice(),
                rc.as_mut_slice(),
            ],
            pos,
        };
        enc.encode(&mut out, src, &mut src_pos);
        pos = out.pos;
        if enc.is_finished() {
            break;
        }
        match enc.full_stream() {
            Some(i) => {
                let grow = (bufs[i].len() / 2).max(64);
                bufs[i].resize(bufs[i].len() + grow, 0);
            }
            // The encoder wants more source and there is none: the finish mode
            // is EndStream, so this cannot happen before the flush.
            None => unreachable!("BCJ2 encoder asked for more input after the end of the stream"),
        }
    }

    let [main, call, jump, rc] = bufs;
    Bcj2Streams {
        main: truncated(main, pos[STREAM_MAIN]),
        call: truncated(call, pos[STREAM_CALL]),
        jump: truncated(jump, pos[STREAM_JUMP]),
        rc: truncated(rc, pos[STREAM_RC]),
    }
}

fn truncated(mut v: Vec<u8>, n: usize) -> Vec<u8> {
    v.truncate(n);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A xorshift, so the inputs are generated rather than committed.
    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0
        }
    }

    /// Bytes shaped like x86: branch opcodes at every alignment, with offsets
    /// on both sides of the encoder's relative limit.
    fn code_like(seed: u32, len: usize) -> Vec<u8> {
        let mut rng = Rng(seed | 1);
        let mut out = Vec::with_capacity(len + 8);
        while out.len() < len {
            let r = rng.next();
            match r % 6 {
                0 => {
                    out.push(0xE8);
                    out.extend_from_slice(&(rng.next() & 0x000F_FFFF).to_le_bytes());
                }
                1 => {
                    out.push(0xE9);
                    out.extend_from_slice(&rng.next().to_le_bytes());
                }
                2 => {
                    out.push(0x0F);
                    out.push(0x80 | (rng.next() & 0x0F) as u8);
                    out.extend_from_slice(&(rng.next() & 0x0000_FFFF).to_le_bytes());
                }
                3 => out.extend_from_slice(&r.to_le_bytes()),
                4 => out.push(0xE8),
                _ => out.push((r >> 11) as u8),
            }
        }
        out.truncate(len);
        out
    }

    fn roundtrip(src: &[u8]) {
        let s = encode_to_streams(src);
        assert!(s.call.len().is_multiple_of(4));
        assert!(s.jump.len().is_multiple_of(4));
        assert_eq!(
            s.main.len() + s.call.len() + s.jump.len(),
            src.len(),
            "the three plain streams must account for every source byte"
        );
        let back = decode_to_vec(&s.main, &s.call, &s.jump, &s.rc, src.len()).expect("decode");
        assert_eq!(back, src, "{} bytes", src.len());
    }

    #[test]
    fn decoding_what_the_encoder_produced_gives_the_input_back() {
        for len in [
            0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 15, 16, 17, 255, 4096, 70_000,
        ] {
            roundtrip(&code_like(0x1234_5678, len));
        }
        let mut rng = Rng(0xDEAD_BEEF);
        let random: Vec<u8> = (0..30_000).map(|_| rng.next() as u8).collect();
        roundtrip(&random);
        roundtrip(&vec![0xE8u8; 5000]);
        roundtrip(&vec![0u8; 5000]);
    }

    /// Every marker byte at every alignment inside a word, which is what
    /// exercises the rolling two-byte context.
    #[test]
    fn markers_at_every_alignment_round_trip() {
        for opcode in [0xE8u8, 0xE9, 0x0F] {
            for second in [0x80u8, 0x8F, 0x00, 0x90] {
                for pad in 0..8usize {
                    let mut buf = vec![0x90u8; pad];
                    buf.push(opcode);
                    buf.push(second);
                    buf.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
                    buf.extend_from_slice(&[0x90; 3]);
                    roundtrip(&buf);
                }
            }
        }
    }

    /// The decoder must give the same bytes whether it is fed everything at
    /// once or a little of each stream at a time.
    #[test]
    fn decoding_in_pieces_is_decoding_whole() {
        let src = code_like(0x0BAD_C0DE, 20_011);
        let s = encode_to_streams(&src);

        for chunk in [1usize, 7, 64, 1000] {
            // The call and jump streams move four bytes at a time.
            let cj_chunk = chunk.next_multiple_of(4);
            let mut dec = Bcj2Dec::new();
            let mut out = vec![0u8; src.len()];
            let (mut m, mut c, mut j, mut r, mut d) = (0usize, 0usize, 0usize, 0usize, 0usize);
            while d < src.len() {
                let (mm, cc, jj, rr) = (
                    (m + chunk).min(s.main.len()),
                    (c + cj_chunk).min(s.call.len()),
                    (j + cj_chunk).min(s.jump.len()),
                    (r + chunk).min(s.rc.len()),
                );
                let dd = (d + chunk).min(out.len());
                let mut st = Bcj2DecStreams::new(
                    &s.main[m..mm],
                    &s.call[c..cc],
                    &s.jump[j..jj],
                    &s.rc[r..rr],
                    &mut out[d..dd],
                );
                dec.decode(&mut st).expect("decode");
                m = mm - st.bufs[STREAM_MAIN].len();
                c = cc - st.bufs[STREAM_CALL].len();
                j = jj - st.bufs[STREAM_JUMP].len();
                r = rr - st.bufs[STREAM_RC].len();
                d += st.dest_pos;
            }
            assert_eq!(out, src, "chunk {chunk}");
        }
    }

    /// And the encoder must give the same four streams whether it is fed the
    /// source whole or in pieces, which is what `temp` and the `CONTINUE`
    /// finish mode are for.
    #[test]
    fn encoding_in_pieces_is_encoding_whole() {
        let src = code_like(0xFEED_FACE, 12_345);
        let whole = encode_to_streams(&src);

        for chunk in [1usize, 2, 3, 5, 7, 64, 4096] {
            let mut enc = Bcj2Enc::new();
            let mut bufs: [Vec<u8>; NUM_STREAMS] = [
                vec![0u8; src.len() + 64],
                vec![0u8; src.len() + 64],
                vec![0u8; src.len() + 64],
                vec![0u8; src.len() + 64],
            ];
            let mut pos = [0usize; NUM_STREAMS];
            let mut at = 0usize;
            loop {
                let end = (at + chunk).min(src.len());
                let last = end == src.len();
                enc.set_finish_mode(if last {
                    Bcj2EncFinishMode::EndStream
                } else {
                    Bcj2EncFinishMode::Continue
                });
                let piece = &src[at..end];
                let mut piece_pos = 0usize;
                {
                    let [m, c, j, r] = &mut bufs;
                    let mut out = Bcj2EncOut {
                        bufs: [
                            m.as_mut_slice(),
                            c.as_mut_slice(),
                            j.as_mut_slice(),
                            r.as_mut_slice(),
                        ],
                        pos,
                    };
                    enc.encode(&mut out, piece, &mut piece_pos);
                    pos = out.pos;
                }
                // The windows are sized so that they cannot fill, which is what
                // makes one call per piece enough.
                assert!(
                    enc.full_stream().is_none(),
                    "the output windows were sized not to fill"
                );
                at = end;
                if last {
                    break;
                }
            }
            assert!(enc.is_finished(), "chunk {chunk}");
            assert_eq!(
                &bufs[STREAM_MAIN][..pos[STREAM_MAIN]],
                &whole.main[..],
                "chunk {chunk} main"
            );
            assert_eq!(
                &bufs[STREAM_CALL][..pos[STREAM_CALL]],
                &whole.call[..],
                "chunk {chunk} call"
            );
            assert_eq!(
                &bufs[STREAM_JUMP][..pos[STREAM_JUMP]],
                &whole.jump[..],
                "chunk {chunk} jump"
            );
            assert_eq!(
                &bufs[STREAM_RC][..pos[STREAM_RC]],
                &whole.rc[..],
                "chunk {chunk} rc"
            );
        }
    }

    /// A relative limit of zero turns conversion off, so the main stream is
    /// the source and the other two are empty.
    #[test]
    fn a_zero_relative_limit_converts_nothing() {
        let src = code_like(0x5EED_0001, 5000);
        let mut enc = Bcj2Enc::new();
        enc.set_relat_limit(0);
        let s = encode_to_streams_with(&mut enc, &src);
        assert_eq!(s.main, src);
        assert!(s.call.is_empty() && s.jump.is_empty());
        let back = decode_to_vec(&s.main, &s.call, &s.jump, &s.rc, src.len()).expect("decode");
        assert_eq!(back, src);
    }

    /// A file size limit keeps the encoder from converting an offset whose
    /// absolute target lands outside the file, and the pair still round-trips.
    #[test]
    fn a_file_size_limit_still_round_trips() {
        let src = code_like(0x5EED_0002, 8000);
        for size in [1u64, 64, 4096, 1 << 20] {
            let mut enc = Bcj2Enc::new();
            enc.set_file_size(size);
            let s = encode_to_streams_with(&mut enc, &src);
            let back = decode_to_vec(&s.main, &s.call, &s.jump, &s.rc, src.len()).expect("decode");
            assert_eq!(back, src, "file size {size}");
        }
    }

    #[test]
    fn a_call_stream_that_is_not_a_multiple_of_four_is_refused() {
        let mut out = [0u8; 16];
        let mut dec = Bcj2Dec::new();
        let mut s = Bcj2DecStreams::new(&[0u8; 4], &[0u8; 3], &[], &[0u8; 5], &mut out);
        assert_eq!(dec.decode(&mut s), Err(Bcj2Error::UnalignedStream));
    }

    #[test]
    fn a_range_coded_stream_that_does_not_start_with_a_zero_is_refused() {
        let mut out = [0u8; 16];
        let mut dec = Bcj2Dec::new();
        // C: the first RC byte must be zero, and five 0xff bytes are refused
        // as well.
        let mut s = Bcj2DecStreams::new(&[0u8; 4], &[], &[], &[1, 0, 0, 0, 0], &mut out);
        assert_eq!(dec.decode(&mut s), Err(Bcj2Error::Data));

        let mut dec = Bcj2Dec::new();
        let mut s =
            Bcj2DecStreams::new(&[0u8; 4], &[], &[], &[0, 0xFF, 0xFF, 0xFF, 0xFF], &mut out);
        assert_eq!(dec.decode(&mut s), Err(Bcj2Error::Data));
    }
}
