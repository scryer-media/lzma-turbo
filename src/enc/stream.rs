//! The input a match finder pulls from.
//!
//! C: `ISeqInStream` in `C/7zTypes.h`, which `CMatchFinder` calls through
//! `p->stream`. The C keeps the pointer in the struct; the port passes it in
//! as a parameter instead, because a borrow held in the struct would put a
//! lifetime on `CLzmaEnc` and on everything that owns one.
//!
//! # Deviation: `directInput` only for a threaded block
//!
//! `MatchFinder_SET_DIRECT_INPUT_BUF` lets the C point `p->buffer` straight at
//! the caller's bytes and never allocate a window. This port does that only
//! where the C's threaded `Lzma2Enc` spends most of its time: a block a block
//! thread codes with the threaded match finder, which reads it in place
//! (`crate::enc::lz_find_mt::run_block`). Everywhere else there is the
//! window, filled from a stream, and a slice source is [`SliceStream`], a
//! stream over that slice.
//!
//! The two find the same matches. The two modes differ only in how much input is
//! visible at once, and that reaches nothing a match depends on:
//! `MatchFinder_SetLimits` derives `lenLimit` from the available bytes but
//! clamps it to `matchMaxLen` while more than `keepSizeAfter` remain, which
//! both modes satisfy until the same last bytes of the data; `posLimit` only
//! decides *when* `MatchFinder_CheckLimits` runs, and that function moves the
//! window, reads, wraps `cyclicBufferPos` at exactly `cyclicBufferSize` and
//! normalizes only at a `pos` wrap over 2^32 — none of which changes the
//! positions a match is found at. What does differ is `expectedDataSize`,
//! which sizes the hash table and so does change the output;
//! `LzmaEnc_MemPrepare` sets it from the source length and so does
//! [`crate::enc::LzmaEncoder::encode_slice`].

use crate::error::Error;

/// A byte source the match finder reads its window from.
///
/// C: `ISeqInStream`. Returning `Ok(0)` is the C's "size == 0" end of stream.
pub trait SeqInStream {
    /// Reads into `buf`, returning how many bytes were written. `Ok(0)` means
    /// the stream has ended.
    ///
    /// # Errors
    ///
    /// Whatever the source reports; the encoder turns it into
    /// [`Error::Read`] and stops.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error>;
}

/// A [`SeqInStream`] over a slice already in memory.
///
/// C: the `directInput` mode of `CMatchFinder`, as a stream.
pub struct SliceStream<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceStream<'a> {
    /// Wraps `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        SliceStream { data, pos: 0 }
    }
}

impl SeqInStream for SliceStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let n = buf.len().min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// C: `CLimitedSeqInStream` in `C/Lzma2Enc.c`, which caps one LZMA2 block's
/// worth of input and records whether the real stream ended inside it.
///
/// Generic over the inner stream so that a `Send` one stays `Send`: the
/// threaded match finder's hash thread takes the *limited* stream, not the
/// caller's, and can only do so when it can be sent. The C has no analogue -
/// `mf->stream` is a pointer either way.
pub(crate) struct LimitedSeqInStream<'s, S: SeqInStream + ?Sized = dyn SeqInStream> {
    pub(crate) real_stream: &'s mut S,
    pub(crate) limit: u64,
    pub(crate) processed: u64,
    pub(crate) finished: bool,
}

impl<'s, S: SeqInStream + ?Sized> LimitedSeqInStream<'s, S> {
    /// C: `LimitedSeqInStream_Init`, plus the assignment of `realStream`.
    pub(crate) fn new(real_stream: &'s mut S) -> Self {
        LimitedSeqInStream {
            real_stream,
            limit: u64::MAX,
            processed: 0,
            finished: false,
        }
    }

    /// C: `LimitedSeqInStream_Init` with a fresh limit, which is what
    /// `Lzma2Enc_EncodeMt1` does at the top of every block.
    pub(crate) fn reset(&mut self, limit: u64) {
        self.limit = limit;
        self.processed = 0;
        self.finished = false;
    }
}

impl<S: SeqInStream + ?Sized> SeqInStream for LimitedSeqInStream<'_, S> {
    /// C: `LimitedSeqInStream_Read`.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let mut size2 = buf.len();
        if self.limit != u64::MAX {
            let rem = self.limit - self.processed;
            if size2 as u64 > rem {
                size2 = rem as usize;
            }
        }
        if size2 != 0 {
            size2 = self.real_stream.read(&mut buf[..size2])?;
            self.finished = size2 == 0;
            self.processed += size2 as u64;
        }
        Ok(size2)
    }
}

/// A stream that is never read.
///
/// The threaded match finder takes the real input over for the length of a
/// block - its hash thread is the only reader - so the encoder's own
/// `code_one_block` argument has nothing left to read. Reaching it would mean
/// the match finder asked the lz thread for bytes, which it never does; the
/// end-of-stream answer is the safe one if it ever did.
#[cfg(feature = "std")]
pub(crate) struct NoStream;

#[cfg(feature = "std")]
impl SeqInStream for NoStream {
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Error> {
        Ok(0)
    }
}

/// The output a range encoder flushes into.
///
/// C: `ISeqOutStream`. The C returns a short write to signal failure; this
/// returns an error instead.
pub trait SeqOutStream {
    /// Writes all of `data`.
    ///
    /// # Errors
    ///
    /// Whatever the sink reports; the encoder turns it into [`Error::Write`].
    fn write(&mut self, data: &[u8]) -> Result<(), Error>;

    /// Writes all of `data` and leaves it empty.
    ///
    /// The block-threaded encoder hands each finished block over through
    /// this, a whole block in one buffer of its own. A sink that queues what
    /// it is given can take the allocation ([`core::mem::take`]) instead of
    /// copying the block; the encoder then allocates the next block's buffer
    /// afresh. The default copies through [`SeqOutStream::write`] and leaves
    /// `data` its capacity, which the encoder uses again.
    ///
    /// C: `ISeqOutStream` has nothing like it; `Lzma2Enc_MtCallback_Write`
    /// passes the block's buffer to `ISeqOutStream_Write` and keeps it.
    ///
    /// # Errors
    ///
    /// As [`SeqOutStream::write`]. `data` is in an unspecified state after an
    /// error.
    fn write_vec(&mut self, data: &mut alloc::vec::Vec<u8>) -> Result<(), Error> {
        self.write(data)?;
        data.clear();
        Ok(())
    }
}

impl SeqOutStream for alloc::vec::Vec<u8> {
    fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.try_reserve(data.len()).map_err(|_| Error::Alloc)?;
        self.extend_from_slice(data);
        Ok(())
    }
}
