//! The sequential reader: one pass, no seeking, concatenated streams.
//!
//! C: `XzUnpacker_Code` in `XzDec.c` driven by `Xz_ReadHeader`/`Xz_ReadIndex`
//! from `XzIn.c`, with the one difference that matters for a `Read` adapter:
//! 7-Zip's unpacker is handed whole buffers by its caller, while this one owns
//! the input buffer and pulls, so the state machine has to be able to stop in
//! the middle of every field. The states below are that machine.
//!
//! Nothing here seeks, so this is the reader for a pipe, a socket, or a file
//! being written as it is read. The index is still verified: it is checked
//! against a running fold of the blocks as they go by (see `IndexFold`),
//! which costs twenty-four bytes of state instead of sixteen bytes per block.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use std::io::{self, Read};

use super::XzOptions;
use super::block::{BlockHeader, MAX_BLOCK_HEADER_SIZE, header_size_from_first_byte};
use super::blockdec::{BlockDecoder, BlockLimits};
use super::check::BlockCheck;
use super::error::{XzError, XzErrorKind};
use super::index::{IndexFold, MAX_INDEX_SIZE};
use super::stream::{
    CheckType, STREAM_FOOTER_SIZE, STREAM_HEADER_SIZE, StreamFlags, StreamFooter, StreamHeader,
};
use super::vli;
use crate::crc::Crc32;

/// The input buffer. Large enough to hold any single field - a block header is
/// at most 1024 bytes - with room left over to keep the LZMA2 decoder fed.
pub(crate) const BUF: usize = 1 << 16;

/// The largest check field the format can describe: reserved ids 13-15 are
/// sixty-four bytes (spec §2.1.1.2).
const MAX_CHECK_SIZE: usize = 64;

/// A buffered pull source that can be asked for a run of bytes.
struct Source<R> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    end: usize,
    eof: bool,
    /// File offset of `buf[pos]`.
    offset: u64,
}

impl<R: Read> Source<R> {
    fn new(inner: R) -> Self {
        Source {
            inner,
            buf: vec![0u8; BUF].into_boxed_slice(),
            pos: 0,
            end: 0,
            eof: false,
            offset: 0,
        }
    }

    fn avail(&self) -> &[u8] {
        &self.buf[self.pos..self.end]
    }

    fn consume(&mut self, n: usize) {
        debug_assert!(n <= self.end - self.pos);
        self.pos += n;
        self.offset += n as u64;
    }

    /// Reads until at least `n` bytes are buffered or the input ends. `n` must
    /// not exceed `BUF`.
    fn fill_at_least(&mut self, n: usize) -> io::Result<()> {
        debug_assert!(n <= BUF);
        if self.end - self.pos >= n {
            return Ok(());
        }
        if self.pos > 0 {
            self.buf.copy_within(self.pos..self.end, 0);
            self.end -= self.pos;
            self.pos = 0;
        }
        while self.end < n && !self.eof {
            match self.inner.read(&mut self.buf[self.end..]) {
                Ok(0) => self.eof = true,
                Ok(k) => self.end += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Fills `dst` exactly, or returns false if the input ended first.
    fn take_into(&mut self, dst: &mut [u8]) -> io::Result<bool> {
        self.fill_at_least(dst.len())?;
        if self.end - self.pos < dst.len() {
            return Ok(false);
        }
        dst.copy_from_slice(&self.buf[self.pos..self.pos + dst.len()]);
        self.consume(dst.len());
        Ok(true)
    }
}

/// Where the reader is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// At a stream header: the file's first byte, or just past padding.
    StreamHeader,
    /// At the byte that is either a block header's size or the index
    /// indicator.
    BlockOrIndex,
    /// Inside a block's compressed data.
    Block,
    /// At a block's padding and check field.
    BlockTail,
    /// Inside the index.
    Index,
    /// At the stream footer.
    Footer,
    /// Past a footer, looking for padding or another stream.
    Padding,
    /// Nothing left.
    Done,
}

/// A `Read` adapter over an `.xz` file.
///
/// Reads every stream in the file by default, as `xz -d` does, and verifies
/// everything the format allows: the header CRC-32s before any field in them
/// is believed, each block's check, each block's declared sizes, and the index
/// against the blocks that were actually there.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::Read;
/// use lzma_turbo::xz::XzReader;
///
/// # fn main() -> std::io::Result<()> {
/// let mut r = XzReader::new(File::open("x.tar.xz")?).with_memory_limit(128 << 20);
/// let mut out = Vec::new();
/// r.read_to_end(&mut out)?;
/// # Ok(())
/// # }
/// ```
pub struct XzReader<R: Read> {
    src: Source<R>,
    opts: XzOptions,
    state: State,
    /// Which stream, counting from zero.
    stream: u64,
    /// Which block of this stream, counting from zero.
    block_no: u64,
    flags: StreamFlags,
    check_size: usize,
    /// The block being decoded.
    dec: Option<BlockDecoder>,
    /// File offset of the current block's header.
    block_start: u64,
    block_header_size: usize,
    /// Size of the index just read, to compare with the footer's.
    index_size: u64,
    /// Total output produced across the whole file.
    total_out: u64,
    /// This stream's blocks, folded.
    fold: IndexFold,
    /// The checks of the blocks decoded so far, when the caller asked for
    /// segments.
    checks: Vec<BlockCheck>,
    /// Bytes of block padding and check field still to be consumed.
    tail_left: usize,
}

impl<R: Read> XzReader<R> {
    /// A reader with the default options.
    pub fn new(inner: R) -> Self {
        Self::with_options(inner, XzOptions::default())
    }

    /// A reader with the given options.
    pub fn with_options(inner: R, opts: XzOptions) -> Self {
        XzReader {
            src: Source::new(inner),
            opts,
            state: State::StreamHeader,
            stream: 0,
            block_no: 0,
            flags: StreamFlags {
                check: CheckType::None,
                raw: [0, 0],
            },
            check_size: 0,
            dec: None,
            block_start: 0,
            block_header_size: 0,
            index_size: 0,
            total_out: 0,
            fold: IndexFold::default(),
            checks: Vec::new(),
            tail_left: 0,
        }
    }

    /// Caps what one block's dictionary may cost.
    #[must_use]
    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.opts.memory_limit = bytes;
        self
    }

    /// Turns the per-block check on or off.
    #[must_use]
    pub fn with_checks(mut self, verify: bool) -> Self {
        self.opts.verify_checks = verify;
        self
    }

    /// Allows a stream whose check this build cannot compute to be decoded
    /// unverified.
    #[must_use]
    pub fn allow_unverifiable(mut self, on: bool) -> Self {
        self.opts.allow_unverifiable = on;
        self
    }

    /// Reads only the first stream, treating anything after it as an error.
    #[must_use]
    pub fn single_stream(mut self) -> Self {
        self.opts.concatenated = false;
        self
    }

    /// Total bytes decoded so far.
    #[must_use]
    pub fn total_out(&self) -> u64 {
        self.total_out
    }

    /// Which stream the reader is in, counting from zero.
    #[must_use]
    pub fn stream_index(&self) -> u64 {
        self.stream
    }

    /// The per-block checks computed so far, in block order.
    ///
    /// Only collected when the options carry a [`crate::ChecksumPlan`]; a
    /// caller that does not want them pays neither the memory nor the work.
    #[must_use]
    pub fn block_checks(&self) -> &[BlockCheck] {
        &self.checks
    }

    /// Gives the underlying reader back.
    pub fn into_inner(self) -> R {
        self.src.inner
    }

    // -- errors ------------------------------------------------------------

    /// A located format error, as an `io::Error` carrying the [`XzError`].
    fn fail(&self, kind: impl Into<XzErrorKind>) -> io::Error {
        let e = match self.state {
            State::Block | State::BlockTail => {
                XzError::in_block(kind, self.stream, self.block_no, self.src.offset)
            }
            _ => XzError::at(kind, self.stream, self.src.offset),
        };
        io::Error::from(e)
    }

    // -- the state machine -------------------------------------------------

    fn stream_header(&mut self) -> io::Result<()> {
        let mut buf = [0u8; STREAM_HEADER_SIZE];
        if !self.src.take_into(&mut buf)? {
            return Err(self.fail(XzErrorKind::TruncatedInput));
        }
        let header = StreamHeader::parse(&buf).map_err(|k| self.fail(k))?;
        let check = header.flags.check;
        if self.opts.verify_checks && !check.is_verifiable() && !self.opts.allow_unverifiable {
            return Err(self.fail(XzErrorKind::UnsupportedCheck));
        }
        self.flags = header.flags;
        self.check_size = check.size();
        debug_assert!(self.check_size <= MAX_CHECK_SIZE);
        self.block_no = 0;
        self.fold = IndexFold::default();
        self.state = State::BlockOrIndex;
        Ok(())
    }

    fn block_or_index(&mut self) -> io::Result<()> {
        let mut first = [0u8; 1];
        if !self.src.take_into(&mut first)? {
            return Err(self.fail(XzErrorKind::TruncatedInput));
        }
        let Some(size) = header_size_from_first_byte(first[0]) else {
            self.state = State::Index;
            return Ok(());
        };
        debug_assert!(size <= MAX_BLOCK_HEADER_SIZE);
        self.block_start = self.src.offset - 1;
        let mut buf = [0u8; MAX_BLOCK_HEADER_SIZE];
        buf[0] = first[0];
        if !self.src.take_into(&mut buf[1..size])? {
            return Err(self.fail(XzErrorKind::TruncatedInput));
        }
        let header = BlockHeader::parse(&buf[..size]).map_err(|k| self.fail(k))?;
        self.block_header_size = size;
        let limits = BlockLimits {
            memory_limit: self.opts.memory_limit,
            uncompressed_size: header.uncompressed_size,
            compressed_size: header.compressed_size,
            max_block_size: self.block_cap(),
        };
        let dec = BlockDecoder::new(
            &header.chain,
            self.flags.check,
            self.total_out,
            &self.opts.plan,
            self.opts.verify_checks,
            limits,
        )
        .map_err(|k| self.fail(k))?;
        self.dec = Some(dec);
        self.state = State::Block;
        Ok(())
    }

    /// What one block may produce, with the file-wide cap taken into account.
    fn block_cap(&self) -> u64 {
        match self.opts.max_unpack_bytes {
            Some(cap) => self
                .opts
                .max_block_size
                .min(cap.saturating_sub(self.total_out)),
            None => self.opts.max_block_size,
        }
    }

    fn block(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.dec.as_ref().is_some_and(BlockDecoder::finished) {
                let packed = self.dec.as_ref().expect("block").packed();
                // Spec §3.3: padding to a multiple of four. The header and
                // every check size are multiples of four already, so the
                // compressed size alone decides it.
                let padding = usize::try_from((4 - (packed % 4)) % 4).expect("0..4");
                self.tail_left = padding + self.check_size;
                self.state = State::BlockTail;
                return Ok(0);
            }
            self.src.fill_at_least(1)?;
            let input_empty = self.src.avail().is_empty();
            let res = {
                let dec = self.dec.as_mut().expect("block");
                dec.decode(self.src.avail(), out)
            };
            let (read, wrote) = res.map_err(|k| self.fail(k))?;
            self.src.consume(read);
            if wrote > 0 {
                self.total_out += wrote as u64;
                if let Some(cap) = self.opts.max_unpack_bytes
                    && self.total_out > cap
                {
                    return Err(self.fail(XzErrorKind::TooMuchOutput { limit: cap }));
                }
                return Ok(wrote);
            }
            if read == 0 && input_empty {
                return Err(self.fail(XzErrorKind::TruncatedInput));
            }
        }
    }

    fn block_tail(&mut self) -> io::Result<()> {
        let mut buf = [0u8; MAX_CHECK_SIZE + 3];
        let n = self.tail_left;
        if !self.src.take_into(&mut buf[..n])? {
            return Err(self.fail(XzErrorKind::TruncatedInput));
        }
        let padding = n - self.check_size;
        if buf[..padding].iter().any(|&b| b != 0) {
            return Err(self.fail(XzErrorKind::BadPadding));
        }

        let dec = self.dec.take().expect("block");
        let packed = dec.packed();
        let unpacked = dec.unpacked();
        let check = dec.finish(&buf[padding..n]).map_err(|k| self.fail(k))?;
        if !self.opts.plan.is_none() {
            self.checks.push(check);
        }

        let unpadded = (self.block_header_size as u64) + packed + self.check_size as u64;
        self.fold
            .push(unpadded, unpacked)
            .map_err(|k| self.fail(k))?;
        self.block_no += 1;
        self.state = State::BlockOrIndex;
        Ok(())
    }

    /// Reads the index, verifying it against the fold of the blocks that were
    /// actually decoded. The indicator byte has already been consumed.
    fn index(&mut self) -> io::Result<()> {
        let index_start = self.src.offset - 1;
        let mut crc = Crc32::new();
        crc.update(&[0x00]);
        let mut size: u64 = 1;

        let count = self.index_vli(&mut crc, &mut size)?;
        let (have_count, have_blocks, have_unpacked, have_digest) = self.fold.finish();
        if count != have_count {
            return Err(self.fail(XzErrorKind::IndexMismatch));
        }

        let mut seen = IndexFold::default();
        for _ in 0..count {
            let unpadded = self.index_vli(&mut crc, &mut size)?;
            let uncompressed = self.index_vli(&mut crc, &mut size)?;
            // Spec §4.3.1: a block is at least eight bytes of header plus one
            // byte of data, so no record can describe fewer than five.
            if unpadded < 5 {
                return Err(self.fail(XzErrorKind::IndexMismatch));
            }
            seen.push(unpadded, uncompressed)
                .map_err(|k| self.fail(k))?;
            if size > MAX_INDEX_SIZE {
                return Err(self.fail(XzErrorKind::IndexMismatch));
            }
        }
        let (_, seen_blocks, seen_unpacked, seen_digest) = seen.finish();
        if seen_digest != have_digest
            || seen_blocks != have_blocks
            || seen_unpacked != have_unpacked
        {
            return Err(self.fail(XzErrorKind::IndexMismatch));
        }

        // Spec §4.4: null padding to a multiple of four.
        while !size.is_multiple_of(4) {
            let mut b = [0u8; 1];
            if !self.src.take_into(&mut b)? {
                return Err(self.fail(XzErrorKind::TruncatedInput));
            }
            if b[0] != 0 {
                return Err(self.fail(XzErrorKind::BadPadding));
            }
            crc.update(&b);
            size += 1;
        }
        let mut stored = [0u8; 4];
        if !self.src.take_into(&mut stored)? {
            return Err(self.fail(XzErrorKind::TruncatedInput));
        }
        if crc.finalize() != u32::from_le_bytes(stored) {
            return Err(self.fail(XzErrorKind::HeaderCrc));
        }
        size += 4;

        debug_assert_eq!(self.src.offset - index_start, size);
        self.index_size = size;
        self.state = State::Footer;
        Ok(())
    }

    /// One VLI of the index, folded into the index's own CRC-32.
    fn index_vli(&mut self, crc: &mut Crc32, size: &mut u64) -> io::Result<u64> {
        self.src.fill_at_least(vli::VLI_MAX_BYTES)?;
        let (value, len) = vli::decode(self.src.avail()).map_err(|k| self.fail(k))?;
        crc.update(&self.src.avail()[..len]);
        self.src.consume(len);
        *size += len as u64;
        Ok(value)
    }

    fn footer(&mut self) -> io::Result<()> {
        let mut buf = [0u8; STREAM_FOOTER_SIZE];
        if !self.src.take_into(&mut buf)? {
            return Err(self.fail(XzErrorKind::TruncatedInput));
        }
        let footer = StreamFooter::parse(&buf).map_err(|k| self.fail(k))?;
        // Spec §2.2: the footer's flags must equal the header's.
        if footer.flags.raw != self.flags.raw {
            return Err(self.fail(XzErrorKind::StreamFlagsMismatch));
        }
        if footer.index_size != self.index_size {
            return Err(self.fail(XzErrorKind::IndexMismatch));
        }
        self.stream += 1;
        self.state = State::Padding;
        Ok(())
    }

    /// Stream padding, then either another stream or the end of the file.
    fn padding(&mut self) -> io::Result<()> {
        loop {
            self.src.fill_at_least(4)?;
            let a = self.src.avail();
            if a.is_empty() {
                self.state = State::Done;
                return Ok(());
            }
            if !self.opts.concatenated {
                return Err(self.fail(XzErrorKind::TrailingGarbage));
            }
            // Spec §5: stream padding is a whole number of null four-byte
            // groups, so anything else here has to be the next stream.
            if a.len() < 4 {
                return Err(self.fail(XzErrorKind::TrailingGarbage));
            }
            if a[..4] == [0, 0, 0, 0] {
                self.src.consume(4);
                continue;
            }
            self.state = State::StreamHeader;
            return Ok(());
        }
    }
}

impl<R: Read> Read for XzReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            match self.state {
                State::Done => return Ok(0),
                State::StreamHeader => self.stream_header()?,
                State::BlockOrIndex => self.block_or_index()?,
                State::Block => {
                    let n = self.block(out)?;
                    if n > 0 {
                        return Ok(n);
                    }
                }
                State::BlockTail => self.block_tail()?,
                State::Index => self.index()?,
                State::Footer => self.footer()?,
                State::Padding => self.padding()?,
            }
        }
    }
}
