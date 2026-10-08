//! `std::io::Write` adapters over the encoders.
//!
//! The decoder side offers [`crate::LzmaReader`], [`crate::xz::XzReader`] and
//! friends as `Read`; these are the mirror image, so that a caller can pipe
//! into a compressor the same way it pipes out of a decompressor.
//!
//! [`Lzma2Writer`] and [`XzWriter`] stream: once more than a dictionary's
//! worth has gone in, the encoder runs on a thread of its own and pulls the
//! input as it arrives (see `super::pipe`), so what they hold is about the
//! encoder's dictionary and tables whatever the input's length. Up to a
//! dictionary they hold the input and compress it at the end, because only
//! then does the data size change the bytes — it shrinks the match finder's
//! hash table — and holding it keeps them what a one-shot encode writes.
//!
//! [`LzmaWriter`] buffers its whole input: the `.lzma` header carries the
//! uncompressed size, which is not known until the writer is closed.

use std::io::{self, Write};

use alloc::vec::Vec;

use crate::error::Error;

use super::lzma2_enc::Lzma2Encoder;
use super::pipe::Lzma2Pipe;
use super::props::LzmaEncProps;

#[cfg(feature = "xz")]
use super::xz_enc::XzEncoder;
#[cfg(feature = "xz")]
use crate::xz::filter::FilterFlags;
#[cfg(feature = "xz")]
use crate::xz::stream::CheckType;

/// Turns an encoder error into the `io::Error` a `Write` must return.
fn io(e: Error) -> io::Error {
    io::Error::other(e)
}

/// Writes `out` to `inner`, removing from it what `inner` accepted, so that a
/// failure part way leaves exactly the unsent tail for the next attempt.
fn send(inner: &mut dyn Write, out: &mut Vec<u8>) -> io::Result<()> {
    let mut done = 0;
    let r = loop {
        if done == out.len() {
            break Ok(());
        }
        match inner.write(&out[done..]) {
            Ok(0) => break Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    out.drain(..done);
    r
}

/// Writes a `.lzma` (LZMA-Alone) file.
///
/// The 13-byte header carries the uncompressed size, so nothing can be
/// written until [`LzmaWriter::finish`] is called.
pub struct LzmaWriter<W: Write> {
    inner: Option<W>,
    props: LzmaEncProps,
    buf: Vec<u8>,
}

impl<W: Write> LzmaWriter<W> {
    /// A writer that will compress everything pushed into it with `props`.
    #[must_use]
    pub fn new(inner: W, props: &LzmaEncProps) -> Self {
        LzmaWriter {
            inner: Some(inner),
            props: *props,
            buf: Vec::new(),
        }
    }

    /// Compresses everything written so far, writes it out, and returns the
    /// wrapped writer.
    ///
    /// # Errors
    ///
    /// Whatever the encoder or the wrapped writer returns.
    pub fn finish(mut self) -> io::Result<W> {
        let mut inner = self.inner.take().expect("finish once");
        let out = super::encode_lzma_alone(&self.buf, &self.props).map_err(io)?;
        inner.write_all(&out)?;
        inner.flush()?;
        Ok(inner)
    }
}

impl<W: Write> Write for LzmaWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf
            .try_reserve(buf.len())
            .map_err(|_| io(Error::Alloc))?;
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Writes a raw LZMA2 stream, the payload an `.xz` block or a 7z coder holds.
///
/// [`Lzma2Writer::properties`] is the single dictionary property byte the
/// container has to carry alongside it.
///
/// It streams: past a dictionary's worth of input the encoder runs on its own
/// thread and what has been compressed is written out as it comes, so the
/// memory held does not grow with the input. The bytes are the same as
/// [`Lzma2Encoder::encode_to_vec`] over the whole input. Where no thread can
/// be started the writer holds the input and compresses it on
/// [`Lzma2Writer::finish`] instead.
pub struct Lzma2Writer<W: Write> {
    inner: Option<W>,
    /// `None` once the pipe has taken it.
    enc: Option<Lzma2Encoder>,
    props_byte: u8,
    /// The most input held before streaming starts: the dictionary.
    threshold: u64,
    /// Whether streaming may start; cleared if a thread could not be.
    may_stream: bool,
    buf: Vec<u8>,
    pipe: Option<Lzma2Pipe>,
    /// Compressed bytes on their way to `inner`.
    out: Vec<u8>,
}

impl<W: Write> Lzma2Writer<W> {
    /// A writer that will compress everything pushed into it with `props`.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if a setting is out of range or `lc + lp` is above 4.
    pub fn new(inner: W, props: &LzmaEncProps) -> Result<Self, Error> {
        let enc = Lzma2Encoder::new(props)?;
        Ok(Lzma2Writer {
            inner: Some(inner),
            props_byte: enc.properties(),
            threshold: u64::from(enc.dict_size()),
            enc: Some(enc),
            may_stream: true,
            buf: Vec::new(),
            pipe: None,
            out: Vec::new(),
        })
    }

    /// The single LZMA2 property byte a decoder needs.
    #[must_use]
    pub fn properties(&self) -> u8 {
        self.props_byte
    }

    /// Starts the encoder's thread and hands it what has been held. If no
    /// thread can be started the writer keeps holding, as it did before.
    fn start_pipe(&mut self) -> io::Result<()> {
        let enc = self.enc.take().ok_or_else(|| io(Error::InternalFailure))?;
        match Lzma2Pipe::start(enc) {
            Ok(mut pipe) => {
                let held = core::mem::take(&mut self.buf);
                pipe.write_owned(held, &mut self.out).map_err(io)?;
                self.pipe = Some(pipe);
            }
            Err(enc) => {
                self.enc = Some(*enc);
                self.may_stream = false;
            }
        }
        Ok(())
    }

    /// Hands what the encoder has produced to the wrapped writer.
    fn drain(&mut self) -> io::Result<()> {
        send(self.inner.as_mut().expect("open"), &mut self.out)
    }

    /// Compresses everything written so far, writes it out, and returns the
    /// wrapped writer.
    ///
    /// # Errors
    ///
    /// Whatever the encoder or the wrapped writer returns.
    pub fn finish(mut self) -> io::Result<W> {
        if let Some(pipe) = self.pipe.take() {
            pipe.finish(&mut self.out).map_err(io)?;
        } else {
            let enc = self
                .enc
                .as_mut()
                .ok_or_else(|| io(Error::InternalFailure))?;
            let out = enc.encode_to_vec(&self.buf).map_err(io)?;
            self.out = out;
        }
        self.drain()?;
        let mut inner = self.inner.take().expect("finish once");
        inner.flush()?;
        Ok(inner)
    }
}

impl<W: Write> Write for Lzma2Writer<W> {
    /// An error from the wrapped writer is returned before any of `buf` is
    /// taken: output an earlier call could not hand over goes out first, and
    /// once `buf` has gone into the encoder a failure to pass its output on
    /// is held for the next call to report, as `Write` asks.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.drain()?;
        let mut data = buf;
        if self.pipe.is_none() {
            let take = if self.may_stream {
                let room = self.threshold.saturating_sub(self.buf.len() as u64);
                core::cmp::min(room, data.len() as u64) as usize
            } else {
                data.len()
            };
            self.buf.try_reserve(take).map_err(|_| io(Error::Alloc))?;
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if data.is_empty() {
                return Ok(buf.len());
            }
            self.start_pipe()?;
            if self.pipe.is_none() {
                // No thread: hold the rest too.
                self.buf
                    .try_reserve(data.len())
                    .map_err(|_| io(Error::Alloc))?;
                self.buf.extend_from_slice(data);
                return Ok(buf.len());
            }
        }
        if let Some(pipe) = &mut self.pipe {
            pipe.write(data, &mut self.out).map_err(io)?;
        }
        // `buf` is taken; what `inner` refuses now stays held, and the next
        // `write`, `flush` or `finish` returns the error.
        let _ = self.drain();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain()?;
        self.inner.as_mut().expect("open").flush()
    }
}

/// Writes an `.xz` stream.
///
/// It streams. With the default single block, an input longer than the
/// dictionary is compressed as it arrives on a thread of the encoder's own and
/// written out as it comes, so what the writer holds is about the encoder's
/// dictionary and tables whatever the input's length; that block's header
/// leaves its sizes to the index, as `xz`'s does. With a block size set, each
/// block is held until it fills and is then compressed and written, so what
/// is held is one block per [`XzWriter::set_threads`] thread plus the index.
/// Where no thread can be started, the single block is held whole and
/// compressed on [`XzWriter::finish`].
#[cfg(feature = "xz")]
pub struct XzWriter<W: Write> {
    inner: Option<W>,
    enc: XzEncoder,
    /// Encoder output the wrapped writer has not accepted yet.
    out: Vec<u8>,
}

#[cfg(feature = "xz")]
impl<W: Write> XzWriter<W> {
    /// A writer with the given LZMA2 settings, a CRC-64 check and one block
    /// for the whole input.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if a setting is out of range or `lc + lp` is above 4.
    pub fn new(inner: W, props: &LzmaEncProps) -> Result<Self, Error> {
        Ok(XzWriter {
            inner: Some(inner),
            enc: XzEncoder::new(props)?,
            out: Vec::new(),
        })
    }

    /// Sets the per-block check. Must be called before anything is written.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] for a check this build cannot compute, or if bytes
    /// have already gone in.
    pub fn set_check(&mut self, check: CheckType) -> Result<(), Error> {
        self.enc.set_check(check)
    }

    /// Sets the non-last filters of every block's chain. Must be called
    /// before anything is written.
    ///
    /// # Errors
    ///
    /// As [`XzEncoder::set_filters`].
    pub fn set_filters(&mut self, filters: &[FilterFlags]) -> Result<(), Error> {
        self.enc.set_filters(filters)
    }

    /// Sets how much one block may decode to. Zero means one block for
    /// everything.
    pub fn set_block_size(&mut self, bytes: u64) {
        self.enc.set_block_size(bytes);
    }

    /// Sets how many blocks may be compressed at once. See
    /// [`XzEncoder::set_threads`]: it changes how fast the stream is written,
    /// not what it contains, and needs a block size to have any effect.
    pub fn set_threads(&mut self, threads: usize) {
        self.enc.set_threads(threads);
    }

    /// Writes the index and footer and returns the wrapped writer.
    ///
    /// # Errors
    ///
    /// Whatever the encoder or the wrapped writer returns.
    pub fn finish(mut self) -> io::Result<W> {
        self.enc.finish().map_err(io)?;
        self.drain()?;
        let mut inner = self.inner.take().expect("finish once");
        inner.flush()?;
        Ok(inner)
    }

    /// Hands whatever the encoder has produced to the wrapped writer, keeping
    /// what it does not accept.
    fn drain(&mut self) -> io::Result<()> {
        if !self.enc.output().is_empty() {
            let mut out = self.enc.take_output();
            if self.out.is_empty() {
                self.out = out;
            } else {
                self.out.append(&mut out);
            }
        }
        send(self.inner.as_mut().expect("open"), &mut self.out)
    }
}

#[cfg(feature = "xz")]
impl<W: Write> Write for XzWriter<W> {
    /// As [`Lzma2Writer`]'s: an error from the wrapped writer is returned
    /// before any of `buf` is taken.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.drain()?;
        self.enc.push(buf).map_err(io)?;
        // `buf` is taken; what `inner` refuses now stays held, and the next
        // `write`, `flush` or `finish` returns the error.
        let _ = self.drain();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.drain()?;
        self.inner.as_mut().expect("open").flush()
    }
}
