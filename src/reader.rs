//! `std::io::Read` adapters over the decoders.
//!
//! C: `C/Util/Lzma/LzmaUtil.c`'s `Decode2` streaming loop, expressed as a
//! `Read` implementation instead of a push loop.

use std::io::{self, Read};

use crate::error::{Error, FinishMode, Status};
use crate::lzma::{LzmaDecoder, LzmaProps};
use crate::lzma_alone::{LZMA_ALONE_HEADER_SIZE, LzmaAloneHeader};
use crate::lzma2::Lzma2Decoder;

/// Input buffer size for the adapters. Large enough that the per-call overhead
/// of the decoder entry point disappears against the work it does.
const IN_BUF_SIZE: usize = 1 << 16;

fn to_io(e: Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

struct Source<R> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    eof: bool,
}

impl<R: Read> Source<R> {
    fn new(inner: R) -> Self {
        Source {
            inner,
            buf: vec![0u8; IN_BUF_SIZE].into_boxed_slice(),
            pos: 0,
            len: 0,
            eof: false,
        }
    }

    /// Ensures at least one buffered byte if the underlying reader has any.
    fn fill(&mut self) -> io::Result<()> {
        if self.pos == self.len && !self.eof {
            self.pos = 0;
            self.len = 0;
            let n = self.inner.read(&mut self.buf)?;
            if n == 0 {
                self.eof = true;
            }
            self.len = n;
        }
        Ok(())
    }
}

/// Streaming LZMA1 decoder over any [`Read`].
pub struct LzmaReader<R> {
    src: Source<R>,
    dec: LzmaDecoder,
    remaining: Option<u64>,
    /// Set once the declared uncompressed size has been produced. The stream
    /// is not finished then: it still has to be shown to *end* there, which
    /// [`LzmaReader::check_end`] does before `done`.
    size_reached: bool,
    done: bool,
}

impl<R: Read> LzmaReader<R> {
    /// Default reader allocation budget: 512 MiB. This bounds decoder buffers,
    /// not the decoded output or memory owned by the input reader.
    pub const DEFAULT_MEMORY_LIMIT: u64 = 512 * 1024 * 1024;

    /// Bytes requested for the rounded dictionary, probability table and input
    /// buffer. Allocator bookkeeping and caller-owned memory are excluded.
    pub fn memory_required(props: LzmaProps) -> u64 {
        props.dic_buf_size() as u64
            + (crate::lzma::consts::lzma_props_get_num_probs(props.lc(), props.lp())
                * std::mem::size_of::<u16>()) as u64
            + IN_BUF_SIZE as u64
    }

    fn check_memory(props: LzmaProps, memory_limit: u64) -> io::Result<()> {
        let required = Self::memory_required(props);
        if required > memory_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("LZMA reader requires {required} bytes, limit is {memory_limit}"),
            ));
        }
        Ok(())
    }

    /// Reads a 13-byte `.lzma` header from `inner` and prepares to decode the
    /// rest of the stream.
    ///
    /// # Errors
    ///
    /// Fails if the header is truncated, unsupported, or exceeds
    /// [`Self::DEFAULT_MEMORY_LIMIT`]. Use [`Self::with_memory_limit`] to
    /// explicitly permit a larger dictionary.
    pub fn new(inner: R) -> io::Result<Self> {
        Self::with_memory_limit(inner, Self::DEFAULT_MEMORY_LIMIT)
    }

    /// Reads a `.lzma` header and checks the buffer budget before allocating.
    /// `u64::MAX` explicitly opts out for trusted input. Output size is not bounded.
    ///
    /// # Errors
    /// Returns an error for an invalid header, an exceeded budget or allocation failure.
    pub fn with_memory_limit(mut inner: R, memory_limit: u64) -> io::Result<Self> {
        let mut header = [0u8; LZMA_ALONE_HEADER_SIZE];
        inner.read_exact(&mut header)?;
        let header = LzmaAloneHeader::parse(&header).map_err(to_io)?;
        Self::check_memory(header.props, memory_limit)?;
        let src = Source::new(inner);
        Ok(LzmaReader {
            src,
            dec: LzmaDecoder::new(header.props).map_err(to_io)?,
            remaining: header.uncompressed_size,
            size_reached: header.uncompressed_size == Some(0),
            done: false,
        })
    }

    /// Builds a reader over a raw LZMA1 stream with properties supplied out of
    /// band, as a 7z coder does.
    ///
    /// # Errors
    ///
    /// Fails if the decoder cannot be allocated. This low-level constructor
    /// keeps its unrestricted allocation policy for container callers that
    /// already enforce their own limits. For untrusted properties use
    /// [`Self::with_props_and_memory_limit`].
    pub fn with_props(
        inner: R,
        props: LzmaProps,
        uncompressed_size: Option<u64>,
    ) -> io::Result<Self> {
        Self::with_props_and_memory_limit(inner, props, uncompressed_size, u64::MAX)
    }

    /// Builds a raw LZMA1 reader after checking its buffer allocation budget.
    ///
    /// # Errors
    /// Returns an error before allocation if the budget would be exceeded.
    pub fn with_props_and_memory_limit(
        inner: R,
        props: LzmaProps,
        uncompressed_size: Option<u64>,
        memory_limit: u64,
    ) -> io::Result<Self> {
        Self::check_memory(props, memory_limit)?;
        Ok(LzmaReader {
            src: Source::new(inner),
            dec: LzmaDecoder::new(props).map_err(to_io)?,
            remaining: uncompressed_size,
            size_reached: uncompressed_size == Some(0),
            done: false,
        })
    }

    /// Checks that the stream really ends where the declared size said it
    /// would, once that many bytes have been produced.
    ///
    /// C: liblzma's `lzma_decode` in `lzma_decoder.c`. Reaching a known
    /// uncompressed size is not the end of the stream by itself: the range
    /// coder is normalised and `rc_is_finished` closes the stream, and
    /// otherwise - `eopm_is_valid` - the next symbol has to be the end
    /// marker, while a literal or a match that would produce more output is
    /// `LZMA_DATA_ERROR`. Here that is one more decode call with no room for
    /// output and [`FinishMode::End`], which is what makes `LzmaDec` take its
    /// `checkEndMarkNow` path, and it has to happen however the input
    /// arrives: a reader fed a byte at a time reaches the size in a call that
    /// ran out of input long before the following symbol was seen.
    fn check_end(&mut self) -> io::Result<()> {
        loop {
            self.src.fill()?;
            let input = &self.src.buf[self.src.pos..self.src.len];
            let progress = self
                .dec
                .decode(input, &mut [], FinishMode::End)
                .map_err(to_io)?;
            self.src.pos += progress.read;
            if matches!(
                progress.status,
                Status::FinishedWithMark | Status::MaybeFinishedWithoutMark
            ) {
                self.size_reached = false;
                self.done = true;
                return Ok(());
            }
            // No progress and nothing more to come, or nothing taken from
            // input that is there: the end cannot be established.
            if progress.read == 0 && (self.src.eof || self.src.pos < self.src.len) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated LZMA stream",
                ));
            }
        }
    }
}

// C: CDecoder::Read compares the 64-bit remainder before narrowing it.
fn output_window(remaining: Option<u64>, capacity: usize) -> (usize, FinishMode) {
    match remaining {
        Some(r) if r <= capacity as u64 => (r as usize, FinishMode::End),
        _ => (capacity, FinishMode::Any),
    }
}

impl<R: Read> Read for LzmaReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.done || out.is_empty() {
            return Ok(0);
        }
        if self.size_reached {
            self.check_end()?;
            return Ok(0);
        }
        let mut written = 0usize;
        while written == 0 {
            self.src.fill()?;
            let input = &self.src.buf[self.src.pos..self.src.len];

            let (limit, finish) = output_window(self.remaining, out.len());

            let progress = self
                .dec
                .decode(input, &mut out[..limit], finish)
                .map_err(to_io)?;
            self.src.pos += progress.read;
            written = progress.written;
            if let Some(r) = self.remaining.as_mut() {
                *r -= written as u64;
                if *r == 0 {
                    self.size_reached = true;
                }
            }

            match progress.status {
                Status::FinishedWithMark => {
                    // C: `eopm_is_valid` in liblzma's `lzma_decoder.c`. The
                    // end marker closes a stream of unknown size, or one whose
                    // declared size has just been reached; arriving with output
                    // still owed makes the stream corrupt, not finished.
                    if self.remaining.is_some_and(|r| r > 0) {
                        return Err(to_io(Error::CorruptData));
                    }
                    self.size_reached = false;
                    self.done = true;
                }
                Status::NeedsMoreInput if self.src.eof && progress.read == 0 && written == 0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated LZMA stream",
                    ));
                }
                _ => {}
            }

            if written == 0 && self.done {
                break;
            }
            if written == 0 && self.size_reached {
                self.check_end()?;
                break;
            }
            if written == 0 && progress.read == 0 && self.src.eof {
                self.done = true;
                break;
            }
        }
        Ok(written)
    }
}

/// Streaming LZMA2 decoder over any [`Read`].
pub struct Lzma2Reader<R> {
    src: Source<R>,
    dec: Lzma2Decoder,
    done: bool,
}

impl<R: Read> Lzma2Reader<R> {
    /// Builds a reader over a raw LZMA2 stream, given the single
    /// dictionary-size property byte that xz and 7z carry for it.
    ///
    /// # Errors
    ///
    /// Fails if the property byte is invalid, the default memory budget is
    /// exceeded, or allocation fails.
    pub fn new(inner: R, dict_prop: u8) -> io::Result<Self> {
        Self::with_memory_limit(inner, dict_prop, Self::DEFAULT_MEMORY_LIMIT)
    }

    /// Default decoder allocation budget, matching [`LzmaReader`].
    pub const DEFAULT_MEMORY_LIMIT: u64 = LzmaReader::<R>::DEFAULT_MEMORY_LIMIT;

    /// Rounded dictionary, maximum LZMA2 probability table and input-buffer bytes.
    ///
    /// # Errors
    /// Returns an error for an invalid dictionary property.
    pub fn memory_required(dict_prop: u8) -> io::Result<u64> {
        if dict_prop > 40 {
            return Err(to_io(crate::Error::UnsupportedProps));
        }
        let dict_size = crate::lzma2::frame::dic_size_from_prop_full(dict_prop);
        let props =
            LzmaProps::new(crate::lzma2::frame::LZMA2_LCLP_MAX, 0, 0, dict_size).map_err(to_io)?;
        Ok(LzmaReader::<R>::memory_required(props))
    }

    /// Checks the memory budget before allocating a raw LZMA2 decoder.
    /// `u64::MAX` explicitly permits unrestricted allocation for trusted input.
    ///
    /// # Errors
    /// Returns an error for invalid properties, an exceeded budget or allocation failure.
    pub fn with_memory_limit(inner: R, dict_prop: u8, memory_limit: u64) -> io::Result<Self> {
        let required = Self::memory_required(dict_prop)?;
        if required > memory_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("LZMA2 reader requires {required} bytes, limit is {memory_limit}"),
            ));
        }
        Ok(Lzma2Reader {
            src: Source::new(inner),
            dec: Lzma2Decoder::new(dict_prop).map_err(to_io)?,
            done: false,
        })
    }
}

impl<R: Read> Lzma2Reader<R> {
    /// Performs one `Read`-equivalent operation without copying its output.
    ///
    /// `None` means the requested read could cross the dictionary boundary;
    /// neither input nor decoder state has changed, so the caller can use
    /// [`Read::read`] instead. The callback sees the same complete successful
    /// read, including its chunk boundary, before the dictionary can be reused.
    /// It may validate that span before passing it to an output sink.
    ///
    /// # Errors
    /// Returns the same source and decoding errors as `read`, without calling
    /// the callback for a failed read. Callback errors pass through after the
    /// successful read has been consumed. Recovery after a decoding error or
    /// a callback panic is not guaranteed.
    pub fn read_borrowed<F>(&mut self, max: usize, sink: &mut F) -> Option<io::Result<usize>>
    where
        F: FnMut(&[u8]) -> io::Result<()> + ?Sized,
    {
        if self.done || max == 0 {
            return Some(Ok(0));
        }
        let pos = self.dec.dic_pos();
        let size = self.dec.dic_buf_size();
        let available = if pos == size { size } else { size - pos };
        if max > available {
            return None;
        }
        Some(self.read_contiguous(max, sink))
    }

    fn read_contiguous<F>(&mut self, max: usize, sink: &mut F) -> io::Result<usize>
    where
        F: FnMut(&[u8]) -> io::Result<()> + ?Sized,
    {
        loop {
            self.src.fill()?;
            self.dec.wrap_dic_pos();
            let start = self.dec.dic_pos();
            let mut used = 0;
            // C: Lzma2Dec_DecodeToBuf, with its copy omitted. Preserve the
            // extra decode step after partial output and commit input only
            // after the complete read succeeds, exactly as Read does.
            let status = loop {
                let before = self.dec.dic_pos();
                let (read, status) = self
                    .dec
                    .decode_block(
                        start + max,
                        &self.src.buf[self.src.pos + used..self.src.len],
                        FinishMode::Any,
                    )
                    .map_err(to_io)?;
                used += read;
                if self.dec.dic_pos() == before || self.dec.dic_pos() - start == max {
                    break status;
                }
            };
            self.src.pos += used;
            let end = self.dec.dic_pos();
            if status == Status::FinishedWithMark {
                self.done = true;
            }
            if end != start {
                sink(self.dec.dic_slice(start, end))?;
                return Ok(end - start);
            }
            if self.done {
                return Ok(0);
            }
            if used == 0 && self.src.eof {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated LZMA2 stream",
                ));
            }
        }
    }
}

impl<R: Read> Read for Lzma2Reader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.done || out.is_empty() {
            return Ok(0);
        }
        let mut written = 0usize;
        while written == 0 {
            self.src.fill()?;
            let input = &self.src.buf[self.src.pos..self.src.len];
            let progress = self
                .dec
                .decode(input, out, FinishMode::Any)
                .map_err(to_io)?;
            self.src.pos += progress.read;
            written = progress.written;

            if progress.status == Status::FinishedWithMark {
                self.done = true;
            }
            if written == 0 {
                if self.done {
                    break;
                }
                if progress.read == 0 && self.src.eof {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated LZMA2 stream",
                    ));
                }
            }
        }
        Ok(written)
    }
}

#[cfg(test)]
mod borrowed_read_tests {
    use super::*;

    fn stored(bytes: &[u8]) -> Vec<u8> {
        let mut packed = Vec::new();
        for (index, chunk) in bytes.chunks(65536).enumerate() {
            packed.push(if index == 0 { 1 } else { 2 });
            packed.extend_from_slice(&((chunk.len() - 1) as u16).to_be_bytes());
            packed.extend_from_slice(chunk);
        }
        packed.push(0);
        packed
    }

    struct Fragmented<'a> {
        bytes: &'a [u8],
        cap: usize,
        interrupt: bool,
    }

    impl Read for Fragmented<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.interrupt {
                self.interrupt = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let n = out.len().min(self.cap).min(self.bytes.len());
            out[..n].copy_from_slice(&self.bytes[..n]);
            self.bytes = &self.bytes[n..];
            Ok(n)
        }
    }

    #[test]
    fn borrowed_reads_preserve_read_chunks_errors_and_unsupported_state() {
        let data: Vec<u8> = (0..140_000).map(|i| (i * 29) as u8).collect();
        let packed = stored(&data);
        for cap in [3, 997, 65536] {
            for prop in [0, 16] {
                for truncate in [0, 1, 41] {
                    let input = &packed[..packed.len() - truncate];
                    let source = || Fragmented {
                        bytes: input,
                        cap,
                        interrupt: true,
                    };
                    let mut oracle = Lzma2Reader::new(source(), prop).unwrap();
                    let mut candidate = Lzma2Reader::new(source(), prop).unwrap();
                    let mut calls = 0;
                    loop {
                        let max = [0, 1, 4093, 65536, 7][calls % 5];
                        calls += 1;
                        let mut expected = vec![0; max];
                        let result = oracle.read(&mut expected);
                        let before = (
                            candidate.src.pos,
                            candidate.src.len,
                            candidate.src.inner.bytes.len(),
                            candidate.dec.dic_pos(),
                        );
                        let mut got = Vec::new();
                        let borrowed = candidate.read_borrowed(max, &mut |bytes| {
                            got.extend_from_slice(bytes);
                            Ok(())
                        });
                        let actual = match borrowed {
                            Some(result) => result,
                            None => {
                                assert_eq!(
                                    before,
                                    (
                                        candidate.src.pos,
                                        candidate.src.len,
                                        candidate.src.inner.bytes.len(),
                                        candidate.dec.dic_pos()
                                    )
                                );
                                let mut buf = vec![0; max];
                                candidate
                                    .read(&mut buf)
                                    .inspect(|&n| got.extend_from_slice(&buf[..n]))
                            }
                        };
                        match (result, actual) {
                            (Ok(n), Ok(m)) => {
                                assert_eq!(n, m);
                                assert_eq!(&expected[..n], got);
                                if n == 0 && max != 0 {
                                    break;
                                }
                            }
                            (Err(a), Err(b)) => {
                                assert_eq!(a.kind(), b.kind());
                                assert_eq!(a.to_string(), b.to_string());
                                assert!(got.is_empty());
                                if a.kind() != io::ErrorKind::Interrupted {
                                    break;
                                }
                            }
                            pair => panic!("different outcomes: {pair:?}"),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn borrowed_callback_errors_commit_only_the_completed_read() {
        let packed = stored(b"abcdefghij");
        let mut reader = Lzma2Reader::new(packed.as_slice(), 0).unwrap();
        let error = reader
            .read_borrowed(4, &mut |_| Err(io::Error::other("sink marker")))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), "sink marker");
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"efghij");
    }

    #[cfg(feature = "enc")]
    #[test]
    fn borrowed_compressed_reads_engage_and_preserve_reset_and_error_chunks() {
        use crate::enc::{LzmaEncProps, encode_lzma2};
        let data: Vec<u8> = (0..17003)
            .map(|i| ((i / 97 + i % 19) % 251) as u8)
            .collect();
        let props = LzmaEncProps {
            dict_size: 1 << 16,
            num_threads: 1,
            ..LzmaEncProps::default()
        };
        let (prop, first) = encode_lzma2(&data, &props).unwrap();
        assert!(
            first[0] >= 0xe0,
            "fixture must contain compressed data with dictionary reset"
        );
        let mut joined = first[..first.len() - 1].to_vec();
        joined.extend_from_slice(&first);
        let mut damaged = joined.clone();
        damaged[first.len() - 1] = 3;
        let truncated = joined[..joined.len() - 8].to_vec();
        for (case, input) in [joined, damaged, truncated].into_iter().enumerate() {
            for cap in [3, 65536] {
                let source = || Fragmented {
                    bytes: &input,
                    cap,
                    interrupt: false,
                };
                let mut oracle = Lzma2Reader::new(source(), prop).unwrap();
                let mut candidate = Lzma2Reader::new(source(), prop).unwrap();
                let mut engaged = 0;
                let mut all = Vec::new();
                loop {
                    let mut expected = vec![0; 17003];
                    let result = oracle.read(&mut expected);
                    let mut got = Vec::new();
                    let actual = candidate
                        .read_borrowed(expected.len(), &mut |bytes| {
                            engaged += 1;
                            got.extend_from_slice(bytes);
                            Ok(())
                        })
                        .expect("both reset streams fit the dictionary");
                    match (result, actual) {
                        (Ok(n), Ok(m)) => {
                            assert_eq!(n, m);
                            assert_eq!(&expected[..n], got);
                            all.extend_from_slice(&got);
                            if n == 0 {
                                break;
                            }
                        }
                        (Err(a), Err(b)) => {
                            assert_eq!(a.kind(), b.kind());
                            assert_eq!(a.to_string(), b.to_string());
                            assert!(got.is_empty());
                            break;
                        }
                        pair => panic!("compressed outcomes differ: {pair:?}"),
                    }
                }
                if case == 0 {
                    assert!(engaged > 0);
                    assert_eq!(all, data.repeat(2));
                }
            }
        }
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    #[test]
    fn lzma2_reader_checks_budget_before_allocation() {
        let required = Lzma2Reader::<&[u8]>::memory_required(0).unwrap();
        assert!(required > 4096 + IN_BUF_SIZE as u64);
        assert!(Lzma2Reader::with_memory_limit(&[][..], 0, required - 1).is_err());
        let mut reader = Lzma2Reader::with_memory_limit(&[0][..], 0, required).unwrap();
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        assert!(output.is_empty());
        assert!(Lzma2Reader::new(&[][..], 40).is_err());
        assert!(Lzma2Reader::with_memory_limit(&[][..], 41, u64::MAX).is_err());
    }

    #[test]
    fn remaining_size_is_compared_without_narrowing() {
        assert_eq!(
            output_window(Some((1u64 << 32) + 1), 2),
            (2, FinishMode::Any)
        );
        assert_eq!(output_window(Some(2), 2), (2, FinishMode::End));
        assert_eq!(output_window(Some(1), 2), (1, FinishMode::End));
        assert_eq!(output_window(None, 2), (2, FinishMode::Any));
    }

    #[test]
    fn reader_budget_includes_rounding_and_probabilities() {
        let props = LzmaProps::new(3, 0, 2, 4097).unwrap();
        let required = LzmaReader::<&[u8]>::memory_required(props);
        assert!(required > 8192 + IN_BUF_SIZE as u64);
        assert!(
            LzmaReader::with_props_and_memory_limit(&[][..], props, None, required - 1).is_err()
        );
        assert!(LzmaReader::with_props_and_memory_limit(&[][..], props, None, required).is_ok());
        let more_probs = LzmaProps::new(4, 0, 2, 4097).unwrap();
        assert!(
            LzmaReader::with_props_and_memory_limit(&[][..], more_probs, None, required).is_err()
        );
    }

    #[test]
    fn maximum_dictionary_is_rejected_without_allocation() {
        let props = LzmaProps::new(3, 0, 2, u32::MAX).unwrap();
        assert!(LzmaReader::<&[u8]>::memory_required(props) >= u64::from(u32::MAX));
        assert!(
            LzmaReader::with_props_and_memory_limit(
                &[][..],
                props,
                None,
                LzmaReader::<&[u8]>::DEFAULT_MEMORY_LIMIT
            )
            .is_err()
        );
    }

    #[test]
    fn limited_header_reader_preserves_decoded_bytes() {
        let bytes = include_bytes!("../tests/data/text.lc4pb1.lzma");
        let mut expected = Vec::new();
        LzmaReader::new(bytes.as_slice())
            .unwrap()
            .read_to_end(&mut expected)
            .unwrap();
        let mut actual = Vec::new();
        LzmaReader::with_memory_limit(bytes.as_slice(), LzmaReader::<&[u8]>::DEFAULT_MEMORY_LIMIT)
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, expected);
        assert!(!actual.is_empty());
        assert!(LzmaReader::with_memory_limit(bytes.as_slice(), 0).is_err());
    }
}
