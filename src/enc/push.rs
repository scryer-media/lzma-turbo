//! Push-driven LZMA and LZMA2 encoders, for a caller that cannot hand the
//! encoder a stream to pull from and cannot start a thread to bridge one.
//!
//! The SDK's encoder is pull-driven: the match finder reads its window from an
//! `ISeqInStream` whenever it runs low, and a read of nothing is the end of the
//! input. A writer that is pushed bytes has no such stream until it is closed,
//! so the other bridges either hold the whole input or run the encoder on a
//! thread. These hold a bounded queue instead and run the encoder's own block
//! loop - `LzmaEnc_CodeOneBlock` for LZMA, `Lzma2EncInt_EncodeSubblock` for an
//! LZMA2 solid block - only while the queue is deep enough that the match
//! finder cannot run dry inside the call.
//!
//! # Why the queue cannot run dry
//!
//! The match finder reads only when its look-ahead has fallen to exactly
//! `keepSizeAfter` bytes (`MatchFinder_CheckLimits`), and one call of the block
//! loop moves it forward by a bounded amount: `LzmaEnc_CodeOneBlock` returns
//! once 2^17 bytes have been coded, an LZMA2 subblock once a chunk's 2 MiB have
//! been, and either overshoots by at most one parse window and one match. A
//! call is started only while the queue alone holds more than that advance
//! plus the look-ahead, so at any read inside it the bytes still to come exceed
//! `keepSizeAfter` and some of them are still in the queue. The source treats a
//! read from an empty, unfinished queue as an internal failure rather than as
//! the end of the input, so a broken bound is an error and never a silently
//! truncated stream.
//!
//! # Why the bytes are the pull encoder's
//!
//! What the match finder finds does not depend on how its reads are cut - see
//! [`crate::enc::stream`] - and these run the same block loop with the same
//! settings as [`crate::LzmaEncoder::encode`] and [`crate::Lzma2Encoder::encode`]
//! over a stream whose length is not announced. Neither is told the input
//! size, so neither sizes its hash table from it.
//!
//! The match finder is always the single-threaded one: these exist for the
//! callers that have no threads. `LzmaEncProps::with_num_threads(2)` is
//! accepted and ignored, and the bytes are the same as with it, because the
//! threaded finder finds the same matches. Nor is it the threaded finder run
//! inline, which one thread otherwise takes: that reads up to a hash block
//! and a bt block ahead of the coder, far past the look-ahead `MARGIN`
//! covers.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::error::Error;

use super::lzma_enc::LzmaEnc;
use super::lzma2_enc::{CONTROL_EOF, Lzma2Encoder, UNPACK_SIZE_MAX};
use super::props::LzmaEncProps;
use super::stream::{SeqInStream, SeqOutStream};

/// Headroom over one call's advance: a parse window (`kNumOpts`, 2 KiB), the
/// longest match and the match finder's look-ahead (at most `fb + 274`), with
/// room to spare.
const MARGIN: usize = 1 << 16;

/// How far one `LzmaEnc_CodeOneBlock` call in the unlimited form may advance
/// before it returns: the C's `processed >= (1 << 17)` check.
const LZMA_STEP: usize = 1 << 17;

/// Queue depth an LZMA call is started at.
const LZMA_QUEUE: usize = LZMA_STEP + MARGIN;

/// Queue depth an LZMA2 subblock is started at: one chunk's unpacked bytes.
const LZMA2_QUEUE: usize = UNPACK_SIZE_MAX as usize + MARGIN;

/// The bounded queue the match finder reads from.
struct PushSource {
    buf: Vec<u8>,
    /// Where the unread bytes of `buf` start.
    start: usize,
    /// Set by `finish`: an empty queue is now the end of the input.
    ended: bool,
    /// The match finder asked for bytes the queue did not have before the
    /// input ended. Reported as [`Error::InternalFailure`].
    starved: bool,
    /// Every byte pushed, for the LZMA2 block's size check.
    total: u64,
}

impl PushSource {
    fn new(cap: usize) -> Result<Self, Error> {
        let mut buf = Vec::new();
        buf.try_reserve_exact(cap).map_err(|_| Error::Alloc)?;
        Ok(PushSource {
            buf,
            start: 0,
            ended: false,
            starved: false,
            total: 0,
        })
    }

    fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    /// Back to what `new` returns, with the queue's allocation kept.
    fn reset(&mut self) {
        self.buf.clear();
        self.start = 0;
        self.ended = false;
        self.starved = false;
        self.total = 0;
    }

    /// Queues as much of `data` as fits under `cap`, returning how much.
    fn fill(&mut self, data: &[u8], cap: usize) -> usize {
        if self.start != 0 {
            self.buf.copy_within(self.start.., 0);
            self.buf.truncate(self.buf.len() - self.start);
            self.start = 0;
        }
        let take = cap.saturating_sub(self.buf.len()).min(data.len());
        // Within the capacity reserved in `new`, so this never allocates.
        self.buf.extend_from_slice(&data[..take]);
        self.total += take as u64;
        take
    }
}

impl SeqInStream for PushSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let n = buf.len().min(self.pending());
        if n == 0 && !buf.is_empty() && !self.ended {
            self.starved = true;
            return Err(Error::InternalFailure);
        }
        buf[..n].copy_from_slice(&self.buf[self.start..self.start + n]);
        self.start += n;
        Ok(n)
    }
}

/// The settings the push encoders run with: the caller's, on the
/// single-threaded match finder.
fn single_finder(props: &LzmaEncProps) -> LzmaEncProps {
    let mut p = *props;
    p.num_threads = 1;
    p
}

/// What a step that failed reports: the queue running dry is this module's
/// bug, whatever the encoder turned it into.
fn step_error(src: &PushSource, err: Error) -> Error {
    if src.starved {
        Error::InternalFailure
    } else {
        err
    }
}

/// A raw LZMA stream encoded from pushed input.
///
/// C: `LzmaEnc_Encode` over a stream, its `LzmaEnc_CodeOneBlock` loop run one
/// call at a time as input arrives. [`LzmaPushEncoder::push`] the input in
/// pieces of any size, then [`LzmaPushEncoder::finish`] once; the bytes written
/// to the sinks, in order, are what [`crate::LzmaEncoder::encode`] writes for
/// the whole input with the same settings. Whether the stream ends with an end
/// marker is the `write_end_mark` setting, as there.
///
/// It holds the encoder's window and tables, as any LZMA encoder does, plus a
/// queue of 192 KiB, whatever the input's length.
pub struct LzmaPushEncoder {
    enc: Box<LzmaEnc>,
    src: PushSource,
    done: bool,
}

impl LzmaPushEncoder {
    /// C: `LzmaEnc_Create`, `LzmaEnc_SetProps` and `LzmaEnc_Prepare`.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if a setting is out of range, [`Error::Alloc`] if the
    /// encoder or its queue could not be allocated.
    pub fn new(props: &LzmaEncProps) -> Result<Self, Error> {
        let mut enc = Box::new(LzmaEnc::new()?);
        enc.inline_finder = false;
        enc.set_props(&single_finder(props))?;
        enc.prepare(0)?;
        Ok(LzmaPushEncoder {
            enc,
            src: PushSource::new(LZMA_QUEUE)?,
            done: false,
        })
    }

    /// Makes this encoder ready for another stream, coded with `props`: what
    /// [`LzmaPushEncoder::new`] would return, but with the queue, the window
    /// and the tables this one already has, wherever the new settings need no
    /// more than they hold. A stream in progress is abandoned where it stands.
    ///
    /// C: `LzmaEnc_SetProps` and `LzmaEnc_Prepare` on an encoder that has been
    /// used. The bytes written from here on are a new encoder's.
    ///
    /// # Errors
    ///
    /// As [`LzmaPushEncoder::new`]. After an error the encoder takes no input
    /// until a later call succeeds.
    pub fn reset(&mut self, props: &LzmaEncProps) -> Result<(), Error> {
        self.done = true;
        self.enc.set_props(&single_finder(props))?;
        self.enc.prepare(0)?;
        self.src.reset();
        self.done = false;
        Ok(())
    }

    /// The five LZMA property bytes a decoder needs for this setting.
    ///
    /// C: `LzmaEnc_WriteProperties`.
    #[must_use]
    pub fn properties(&self) -> [u8; crate::lzma::consts::LZMA_PROPS_SIZE] {
        self.enc.write_properties()
    }

    /// Adds the next input bytes, writing to `out` whatever the encoder
    /// produces on the way.
    ///
    /// # Errors
    ///
    /// Whatever `out` returns, [`Error::Param`] after
    /// [`LzmaPushEncoder::finish`] or after an earlier error.
    pub fn push(&mut self, mut data: &[u8], out: &mut dyn SeqOutStream) -> Result<(), Error> {
        if self.done {
            return Err(Error::Param);
        }
        while !data.is_empty() {
            let took = self.src.fill(data, LZMA_QUEUE);
            data = &data[took..];
            while self.src.pending() >= LZMA_QUEUE {
                if let Err(err) = self.step(out) {
                    self.done = true;
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    /// One `LzmaEnc_CodeOneBlock` call mid-stream.
    fn step(&mut self, out: &mut dyn SeqOutStream) -> Result<(), Error> {
        self.enc
            .code_one_block(&mut self.src, out, 0, 0)
            .map_err(|e| step_error(&self.src, e))?;
        if self.enc.finished {
            // The stream can only end at an empty match finder, which a
            // queue this deep rules out.
            return Err(Error::InternalFailure);
        }
        Ok(())
    }

    /// Ends the input and writes the rest of the stream to `out`.
    ///
    /// # Errors
    ///
    /// As [`LzmaPushEncoder::push`]; a second call is [`Error::Param`].
    pub fn finish(&mut self, out: &mut dyn SeqOutStream) -> Result<(), Error> {
        if self.done {
            return Err(Error::Param);
        }
        self.done = true;
        self.src.ended = true;
        loop {
            self.enc
                .code_one_block(&mut self.src, out, 0, 0)
                .map_err(|e| step_error(&self.src, e))?;
            if self.enc.finished {
                return Ok(());
            }
        }
    }
}

/// A raw LZMA2 stream, one solid block, encoded from pushed input.
///
/// C: `Lzma2Enc_EncodeMt1` over a stream with `blockSize` solid, its
/// `Lzma2EncInt_EncodeSubblock` loop run one chunk at a time as input arrives.
/// [`Lzma2PushEncoder::push`] the input in pieces of any size, then
/// [`Lzma2PushEncoder::finish`] once; the bytes written to the sinks, in order,
/// are what [`crate::Lzma2Encoder::encode`] writes for the whole input with the
/// same settings and no data size, ending in the end-of-stream control byte.
///
/// It holds the encoder's window and tables, as the single-threaded LZMA2
/// encoder does, plus a queue of one chunk (2 MiB) and 64 KiB, whatever the
/// input's length.
pub struct Lzma2PushEncoder {
    enc: Lzma2Encoder,
    src: PushSource,
    /// Whether the block has been opened: the window and tables allocated and
    /// the encoder initialised. Left until the first chunk is coded, so that
    /// an input that ends before the queue ever fills is known whole by then.
    begun: bool,
    done: bool,
}

impl Lzma2PushEncoder {
    /// C: `Lzma2Enc_Create`, `Lzma2Enc_SetProps` and the head of
    /// `Lzma2Enc_EncodeMt1`'s block loop.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if a setting is out of range - including `lc + lp`
    /// above 4, which LZMA2 does not allow - and [`Error::Alloc`] if the
    /// encoder or its queue could not be allocated. The window and the match
    /// finder's tables are allocated when the first chunk is coded, so a
    /// failure to allocate those is reported by the [`Lzma2PushEncoder::push`]
    /// or [`Lzma2PushEncoder::finish`] that codes it.
    pub fn new(props: &LzmaEncProps) -> Result<Self, Error> {
        // Solid and one block thread are `Lzma2Encoder`'s defaults.
        let mut enc = Lzma2Encoder::new(&single_finder(props))?;
        enc.sync_coder()?;
        enc.coder.set_inline_finder(false);
        // What `LzmaEnc_Alloc` would refuse, refused here and not at the
        // first chunk.
        enc.coder.check_alloc()?;
        Ok(Lzma2PushEncoder {
            enc,
            src: PushSource::new(LZMA2_QUEUE)?,
            begun: false,
            done: false,
        })
    }

    /// Makes this encoder ready for another stream, coded with `props`: what
    /// [`Lzma2PushEncoder::new`] would return, but with the queue, the window
    /// and the tables this one already has, wherever the new settings need no
    /// more than they hold. A stream in progress is abandoned where it stands.
    ///
    /// C: `Lzma2Enc_SetProps` on a handle that has been used; the block loop's
    /// `LzmaEnc_Alloc` and `LzmaEnc_Init` then open the next stream as they
    /// open every block. The bytes written from here on are a new encoder's.
    ///
    /// # Errors
    ///
    /// As [`Lzma2PushEncoder::new`]. After an error the encoder takes no input
    /// until a later call succeeds.
    pub fn reset(&mut self, props: &LzmaEncProps) -> Result<(), Error> {
        self.done = true;
        self.enc.set_props(&single_finder(props))?;
        self.enc.coder.set_inline_finder(false);
        // What `LzmaEnc_Alloc` would refuse, refused here as `new` refuses it.
        self.enc.coder.check_alloc()?;
        self.src.reset();
        self.begun = false;
        self.done = false;
        Ok(())
    }

    /// C: the head of `Lzma2Enc_EncodeMt1`'s block loop, run once before the
    /// first chunk. `data_limit` is the whole input's length when it is
    /// already known, which is when `finish` arrives before the queue has
    /// filled once.
    fn begin(&mut self, data_limit: u64) -> Result<(), Error> {
        if !self.begun {
            // `set_props` may have built a new coder since `new` or `reset`.
            self.enc.coder.set_inline_finder(false);
            self.enc.coder.begin_solid(data_limit)?;
            self.begun = true;
        }
        Ok(())
    }

    /// The single LZMA2 property byte a decoder needs.
    ///
    /// C: `Lzma2Enc_WriteProperties`.
    #[must_use]
    pub fn properties(&self) -> u8 {
        self.enc.properties()
    }

    /// The dictionary size the property byte rounds up to.
    #[must_use]
    pub fn dict_size(&self) -> u32 {
        self.enc.dict_size()
    }

    /// Adds the next input bytes, writing to `out` whatever the encoder
    /// produces on the way.
    ///
    /// # Errors
    ///
    /// Whatever `out` returns, [`Error::Param`] after
    /// [`Lzma2PushEncoder::finish`] or after an earlier error.
    pub fn push(&mut self, mut data: &[u8], out: &mut dyn SeqOutStream) -> Result<(), Error> {
        if self.done {
            return Err(Error::Param);
        }
        while !data.is_empty() {
            let took = self.src.fill(data, LZMA2_QUEUE);
            data = &data[took..];
            while self.src.pending() >= LZMA2_QUEUE {
                if let Err(err) = self.step(out) {
                    self.done = true;
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    /// One `Lzma2EncInt_EncodeSubblock` call mid-stream.
    fn step(&mut self, out: &mut dyn SeqOutStream) -> Result<(), Error> {
        self.begin(u64::MAX)?;
        let written = self
            .enc
            .coder
            .encode_subblock(&mut self.src, out)
            .map_err(|e| step_error(&self.src, e))?;
        if written == 0 {
            // A subblock is empty only at the end of the input.
            return Err(Error::InternalFailure);
        }
        Ok(())
    }

    /// Ends the input and writes the rest of the stream, with its
    /// end-of-stream control byte, to `out`.
    ///
    /// # Errors
    ///
    /// As [`Lzma2PushEncoder::push`]; a second call is [`Error::Param`].
    pub fn finish(&mut self, out: &mut dyn SeqOutStream) -> Result<(), Error> {
        if self.done {
            return Err(Error::Param);
        }
        self.done = true;
        self.src.ended = true;
        // Nothing coded yet: every byte of the input is in the queue, and the
        // queue is now all the match finder can be given.
        self.begin(self.src.total)?;
        loop {
            let written = self
                .enc
                .coder
                .encode_subblock(&mut self.src, out)
                .map_err(|e| step_error(&self.src, e))?;
            if written == 0 {
                break;
            }
        }
        // C: `if (p->srcPos != limitedInStream.processed) return SZ_ERROR_FAIL`.
        if self.enc.coder.src_pos() != self.src.total {
            return Err(Error::InternalFailure);
        }
        out.write(&[CONTROL_EOF])
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{LZMA_QUEUE, LZMA2_QUEUE, Lzma2PushEncoder, LzmaPushEncoder};
    use crate::enc::{Lzma2Encoder, LzmaEncProps, LzmaEncoder, SliceStream};
    use crate::error::Error;

    /// Compressible with a slow drift, so chunks are LZMA-coded and long
    /// matches cross the step boundaries.
    fn text(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| ((i % 251) as u8).wrapping_add((i / 4093) as u8) ^ ((i / 65_521) as u8))
            .collect()
    }

    /// Incompressible, so the LZMA2 stored-chunk path runs too.
    fn noise(len: usize) -> Vec<u8> {
        let mut x = 0x9E37_79B9u32;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    fn props() -> LzmaEncProps {
        LzmaEncProps::new().with_level(1).with_dict_size(1 << 16)
    }

    fn pull_lzma(props: &LzmaEncProps, src: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        LzmaEncoder::new(props)
            .unwrap()
            .encode(&mut SliceStream::new(src), &mut out)
            .unwrap();
        out
    }

    fn pull_lzma2(props: &LzmaEncProps, src: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        Lzma2Encoder::new(props)
            .unwrap()
            .encode(&mut SliceStream::new(src), &mut out)
            .unwrap();
        out
    }

    fn push_lzma(props: &LzmaEncProps, src: &[u8], piece: &mut dyn FnMut() -> usize) -> Vec<u8> {
        let mut enc = LzmaPushEncoder::new(props).unwrap();
        let mut out = Vec::new();
        let mut rest = src;
        while !rest.is_empty() {
            let n = piece().min(rest.len());
            enc.push(&rest[..n], &mut out).unwrap();
            rest = &rest[n..];
        }
        enc.finish(&mut out).unwrap();
        out
    }

    fn push_lzma2(props: &LzmaEncProps, src: &[u8], piece: &mut dyn FnMut() -> usize) -> Vec<u8> {
        let mut enc = Lzma2PushEncoder::new(props).unwrap();
        let mut out = Vec::new();
        let mut rest = src;
        while !rest.is_empty() {
            let n = piece().min(rest.len());
            enc.push(&rest[..n], &mut out).unwrap();
            rest = &rest[n..];
        }
        enc.finish(&mut out).unwrap();
        out
    }

    /// Lengths either side of the queue depths, the look-ahead and the LZMA2
    /// chunk, and several chunks long.
    fn lengths(queue: usize) -> Vec<usize> {
        let mut v = alloc::vec![1, 2, 273, 547, 548, 4096, 70_000];
        for edge in [queue, queue + 547, 2 << 20, (2 << 20) + 547] {
            v.extend([edge - 1, edge, edge + 1]);
        }
        v.push(7 * (1 << 20) + 12_345);
        v
    }

    #[test]
    fn lzma2_matches_the_pull_encoder() {
        let p = props();
        for len in lengths(LZMA2_QUEUE) {
            for src in [text(len), noise(len)] {
                let want = pull_lzma2(&p, &src);
                assert_eq!(
                    push_lzma2(&p, &src, &mut || src.len()),
                    want,
                    "len {len}, whole"
                );
                assert_eq!(
                    push_lzma2(&p, &src, &mut || 1 << 20),
                    want,
                    "len {len}, 1 MiB"
                );
            }
        }
    }

    #[test]
    fn lzma_matches_the_pull_encoder() {
        for p in [props(), props().with_end_mark(true)] {
            for len in lengths(LZMA_QUEUE) {
                let src = text(len);
                let want = pull_lzma(&p, &src);
                assert_eq!(
                    push_lzma(&p, &src, &mut || src.len()),
                    want,
                    "len {len}, whole"
                );
                assert_eq!(
                    push_lzma(&p, &src, &mut || 65_536),
                    want,
                    "len {len}, 64 KiB"
                );
            }
        }
    }

    /// The optimal parser and a big dictionary, where the window is larger
    /// than the input.
    #[test]
    fn a_large_dictionary_and_the_normal_parser() {
        let p = LzmaEncProps::new().with_level(5).with_dict_size(1 << 24);
        let src = text(5 * (1 << 20) + 3);
        assert_eq!(push_lzma2(&p, &src, &mut || 300_001), pull_lzma2(&p, &src));
        assert_eq!(push_lzma(&p, &src, &mut || 300_001), pull_lzma(&p, &src));
    }

    #[test]
    fn empty_input() {
        let p = props();
        let mut out = Vec::new();
        let mut enc = Lzma2PushEncoder::new(&p).unwrap();
        enc.finish(&mut out).unwrap();
        assert_eq!(out, pull_lzma2(&p, &[]));

        let mut out = Vec::new();
        let mut enc = LzmaPushEncoder::new(&p).unwrap();
        enc.push(&[], &mut out).unwrap();
        enc.finish(&mut out).unwrap();
        assert_eq!(out, pull_lzma(&p, &[]));
    }

    /// Slices of one to seven bytes, so every queue refill is a partial one.
    #[test]
    fn tiny_slices() {
        let p = props();
        let src = text(LZMA2_QUEUE + 300_007);
        let mut k = 0usize;
        let mut piece = move || {
            k += 1;
            1 + k % 7
        };
        assert_eq!(push_lzma2(&p, &src, &mut piece), pull_lzma2(&p, &src));
        let src = text(LZMA_QUEUE * 2 + 99);
        assert_eq!(push_lzma(&p, &src, &mut piece), pull_lzma(&p, &src));
    }

    /// The queue never grows past its depth, however large a push is.
    #[test]
    fn the_queue_is_bounded() {
        let p = props();
        let src = text(9 << 20);
        let mut out = Vec::new();
        let mut enc = Lzma2PushEncoder::new(&p).unwrap();
        enc.push(&src, &mut out).unwrap();
        assert!(enc.src.buf.capacity() <= LZMA2_QUEUE);
        assert!(enc.src.pending() < LZMA2_QUEUE);
        assert!(!out.is_empty(), "chunks are written before finish");
        enc.finish(&mut out).unwrap();
        assert_eq!(out, pull_lzma2(&p, &src));
    }

    #[test]
    fn finish_once() {
        let p = props();
        let mut out = Vec::new();
        let mut enc = Lzma2PushEncoder::new(&p).unwrap();
        enc.finish(&mut out).unwrap();
        assert_eq!(enc.finish(&mut out), Err(Error::Param));
        assert_eq!(enc.push(b"x", &mut out), Err(Error::Param));
        let mut enc = LzmaPushEncoder::new(&p).unwrap();
        enc.finish(&mut out).unwrap();
        assert_eq!(enc.finish(&mut out), Err(Error::Param));
    }

    #[test]
    fn lzma2_refuses_lc_plus_lp_above_four() {
        let p = LzmaEncProps::new().with_lclppb(4, 4, 2);
        assert!(matches!(Lzma2PushEncoder::new(&p), Err(Error::Param)));
    }

    /// Settings that differ in the dictionary, the parser, the finder, the
    /// literal coder and the match length, over inputs that end before the
    /// queue has filled, after it, and not at all.
    fn reset_steps() -> Vec<(LzmaEncProps, Vec<u8>)> {
        let big = LzmaEncProps::new().with_level(5).with_dict_size(1 << 20);
        alloc::vec![
            (big, text(LZMA2_QUEUE + 200_003)),
            (props(), noise(70_000)),
            (
                LzmaEncProps::new().with_level(5).with_dict_size(1 << 12),
                text(100)
            ),
            (props().with_lclppb(0, 2, 2), text(LZMA_QUEUE + 5)),
            (big, Vec::new()),
            (
                LzmaEncProps::new()
                    .with_level(9)
                    .with_dict_size(1 << 16)
                    .with_fast_bytes(273),
                text(300_000)
            ),
        ]
    }

    /// One encoder, reset from stream to stream, writes for each what an
    /// encoder built for it writes, and once it has coded the largest of them
    /// it allocates nothing more.
    #[test]
    fn a_reset_lzma2_encoder_writes_what_a_new_one_writes() {
        let steps = reset_steps();
        let mut enc = Lzma2PushEncoder::new(&steps[0].0).unwrap();
        let mut allocs = 0;
        for pass in 0..2 {
            for (i, (p, src)) in steps.iter().enumerate() {
                if pass + i > 0 {
                    enc.reset(p).unwrap();
                }
                let mut out = Vec::new();
                for piece in src.chunks(300_001) {
                    enc.push(piece, &mut out).unwrap();
                }
                enc.finish(&mut out).unwrap();
                let fresh = Lzma2PushEncoder::new(p).unwrap();
                assert_eq!(enc.properties(), fresh.properties(), "step {i}");
                assert_eq!(enc.dict_size(), fresh.dict_size(), "step {i}");
                assert!(out == pull_lzma2(p, src), "pass {pass}, step {i}");
                if pass == 1 {
                    assert_eq!(enc.enc.coder.finder_allocs(), allocs, "step {i}");
                }
            }
            allocs = enc.enc.coder.finder_allocs();
        }
        assert!(allocs > 0);
    }

    #[test]
    fn a_reset_lzma_encoder_writes_what_a_new_one_writes() {
        let steps = reset_steps();
        let mut enc = LzmaPushEncoder::new(&steps[0].0).unwrap();
        let mut allocs = 0;
        for pass in 0..2 {
            for (i, (p, src)) in steps.iter().enumerate() {
                if pass + i > 0 {
                    enc.reset(p).unwrap();
                }
                let mut out = Vec::new();
                for piece in src.chunks(65_537) {
                    enc.push(piece, &mut out).unwrap();
                }
                enc.finish(&mut out).unwrap();
                let fresh = LzmaPushEncoder::new(p).unwrap();
                assert_eq!(enc.properties(), fresh.properties(), "step {i}");
                assert!(out == pull_lzma(p, src), "pass {pass}, step {i}");
                if pass == 1 {
                    assert_eq!(enc.enc.mf.cfg().allocs, allocs, "step {i}");
                }
            }
            allocs = enc.enc.mf.cfg().allocs;
        }
        assert!(allocs > 0);
    }

    /// A reset abandons a stream that is under way, reopens an encoder that
    /// has finished, and after a setting it refuses leaves the encoder closed
    /// until one it accepts.
    #[test]
    fn a_reset_abandons_the_stream_under_way() {
        let p = props();
        let long = text(LZMA2_QUEUE + 300_007);
        let short = &long[..70_000];

        let mut enc = Lzma2PushEncoder::new(&p).unwrap();
        let mut out = Vec::new();
        enc.push(&long, &mut out).unwrap();
        assert!(!out.is_empty(), "chunks were written and more are queued");
        enc.reset(&p).unwrap();
        let mut out = Vec::new();
        enc.push(short, &mut out).unwrap();
        enc.finish(&mut out).unwrap();
        assert!(out == pull_lzma2(&p, short));
        assert_eq!(enc.push(b"x", &mut out), Err(Error::Param));
        let refused = LzmaEncProps::new().with_lclppb(4, 4, 2);
        assert_eq!(enc.reset(&refused), Err(Error::Param));
        assert_eq!(enc.push(b"x", &mut out), Err(Error::Param));
        assert_eq!(enc.finish(&mut out), Err(Error::Param));
        enc.reset(&p).unwrap();
        let mut out = Vec::new();
        enc.push(short, &mut out).unwrap();
        enc.finish(&mut out).unwrap();
        assert!(out == pull_lzma2(&p, short));

        // Incompressible, so the range coder's buffer has been written out.
        let long = noise(LZMA_QUEUE * 3);
        let mut enc = LzmaPushEncoder::new(&p).unwrap();
        let mut out = Vec::new();
        enc.push(&long, &mut out).unwrap();
        assert!(!out.is_empty(), "blocks were written and more are queued");
        enc.reset(&p).unwrap();
        let mut out = Vec::new();
        enc.push(short, &mut out).unwrap();
        enc.finish(&mut out).unwrap();
        assert!(out == pull_lzma(&p, short));
        assert_eq!(enc.push(b"x", &mut out), Err(Error::Param));
        let refused = LzmaEncProps::new().with_lclppb(9, 0, 2);
        assert_eq!(enc.reset(&refused), Err(Error::Param));
        assert_eq!(enc.push(b"x", &mut out), Err(Error::Param));
        enc.reset(&p).unwrap();
        let mut out = Vec::new();
        enc.push(short, &mut out).unwrap();
        enc.finish(&mut out).unwrap();
        assert!(out == pull_lzma(&p, short));
    }
}
