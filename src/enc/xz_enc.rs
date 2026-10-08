//! The `.xz` container writer.
//!
//! There is no `.xz` encoder in the LZMA SDK — 7-Zip writes `.xz` from
//! `C/XzEnc.c`, which is a thin frame around `Lzma2Enc.c`, and this is the
//! same frame written against the format specification (`xz-file-format.txt`
//! version 1.2.1) and against what [`crate::xz`] already parses. Every field
//! below names the spec section it comes from; the compressed data itself is
//! the port in [`super::lzma2_enc`].
//!
//! What it writes is one stream: a header, one or more blocks, an index over
//! them, and a footer. A block written from a buffer declares both of its
//! sizes in its header; the one solid block of an input too long to hold is
//! compressed as it arrives instead, so its header is written before either
//! size is known and leaves them out, as spec §3.1.2 allows and `xz` itself
//! does — the index still records both. The filter chain is a
//! bare LZMA2 filter unless the caller asks for more with
//! [`XzEncoder::set_filters`], in which case the delta and BCJ converters run
//! over each block before LZMA2 sees it and the block header lists them in
//! that order. Which filter suits which file is a policy question the format
//! does not answer and `xz` only answers with command-line flags, so this
//! writer does not choose for the caller either.

use alloc::vec::Vec;

use crate::error::Error;
use crate::xz::filter::{Converters, FILTER_LZMA2, FilterChain, FilterFlags, MAX_FILTERS};
use crate::xz::stream::{CheckType, XZ_FOOTER_MAGIC, XZ_MAGIC};
use crate::xz::vli;

use super::lzma2_enc::Lzma2Encoder;
use super::pipe::Lzma2Pipe;
use super::props::LzmaEncProps;

/// The default block size: what one block may decode to before the writer
/// starts another.
///
/// `xz` sizes its blocks from the dictionary (`--block-size` otherwise), and
/// only when it is compressing in parallel; single-threaded it writes one
/// block for the whole file. This writer defaults to the same single block,
/// because a block boundary costs compression ratio — the dictionary resets
/// across it — and buys only parallel decoding, which is the caller's call.
pub const DEFAULT_BLOCK_SIZE: u64 = u64::MAX;

/// The largest block header the format allows. Spec §3.1.1.
const MAX_BLOCK_HEADER_SIZE: usize = 1024;

/// How many bytes a check of this type occupies.
fn check_size(check: CheckType) -> usize {
    check.size()
}

/// Computes one block's check over its uncompressed bytes.
///
/// The check types are exactly the ones this build can compute: spec §2.1.1.2
/// allows a decoder to skip a check it does not know, but a writer may not
/// write one it cannot produce, so an unavailable type is a parameter error
/// rather than a silently weaker stream.
fn compute_check(check: CheckType, data: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    match check {
        CheckType::None => {}
        CheckType::Crc32 => out.extend_from_slice(&crate::crc::crc32(data).to_le_bytes()),
        CheckType::Crc64 => out.extend_from_slice(&crate::crc::crc64_xz(data).to_le_bytes()),
        #[cfg(any(feature = "crypto", feature = "native-crypto"))]
        CheckType::Sha256 => {
            let mut h = crate::crypto::Sha256::new();
            h.update(data);
            out.extend_from_slice(&h.finalize());
        }
        #[cfg(not(any(feature = "crypto", feature = "native-crypto")))]
        CheckType::Sha256 => return Err(Error::Param),
        CheckType::Reserved(_) => return Err(Error::Param),
    }
    Ok(())
}

/// One block's check, computed as its uncompressed bytes go by.
enum RunningCheck {
    None,
    Crc32(crate::crc::Crc32),
    Crc64(crate::crc::Crc64Xz),
    #[cfg(any(feature = "crypto", feature = "native-crypto"))]
    Sha256(crate::crypto::Sha256),
}

impl RunningCheck {
    fn new(check: CheckType) -> Result<Self, Error> {
        Ok(match check {
            CheckType::None => RunningCheck::None,
            CheckType::Crc32 => RunningCheck::Crc32(crate::crc::Crc32::new()),
            CheckType::Crc64 => RunningCheck::Crc64(crate::crc::Crc64Xz::new()),
            #[cfg(any(feature = "crypto", feature = "native-crypto"))]
            CheckType::Sha256 => RunningCheck::Sha256(crate::crypto::Sha256::new()),
            #[cfg(not(any(feature = "crypto", feature = "native-crypto")))]
            CheckType::Sha256 => return Err(Error::Param),
            CheckType::Reserved(_) => return Err(Error::Param),
        })
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            RunningCheck::None => {}
            RunningCheck::Crc32(h) => h.update(data),
            RunningCheck::Crc64(h) => h.update(data),
            #[cfg(any(feature = "crypto", feature = "native-crypto"))]
            RunningCheck::Sha256(h) => h.update(data),
        }
    }

    /// Appends the finished check, as [`compute_check`] would have.
    fn finish(self, out: &mut Vec<u8>) {
        match self {
            RunningCheck::None => {}
            RunningCheck::Crc32(h) => out.extend_from_slice(&h.finalize().to_le_bytes()),
            RunningCheck::Crc64(h) => out.extend_from_slice(&h.finalize().to_le_bytes()),
            #[cfg(any(feature = "crypto", feature = "native-crypto"))]
            RunningCheck::Sha256(h) => out.extend_from_slice(&h.finalize()),
        }
    }
}

/// The solid block while it is being compressed as it arrives.
///
/// The header has gone out already, without sizes; the input is checked and
/// run through the filters here, on the caller's thread, and the LZMA2 encoder
/// pulls it from a [`Lzma2Pipe`]. Nothing here grows with the input.
struct SolidStream {
    pipe: Lzma2Pipe,
    check: RunningCheck,
    /// Fresh converters for the block, `None` for a bare LZMA2 chain.
    convs: Option<Converters>,
    /// What the converters made of the last write.
    converted: Vec<u8>,
    header_len: usize,
    uncompressed: u64,
}

impl SolidStream {
    fn push(&mut self, data: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        // §3.4: the check covers what came in, before the filters.
        self.check.update(data);
        self.uncompressed += data.len() as u64;
        match &mut self.convs {
            Some(convs) => {
                self.converted.clear();
                convs.encode_push(data, &mut self.converted);
                self.pipe.write(&self.converted, out)
            }
            None => self.pipe.write(data, out),
        }
    }
}

/// Whether this build can compute `check` at all.
fn check_supported(check: CheckType) -> bool {
    match check {
        CheckType::None | CheckType::Crc32 | CheckType::Crc64 => true,
        CheckType::Sha256 => cfg!(any(feature = "crypto", feature = "native-crypto")),
        CheckType::Reserved(_) => false,
    }
}

/// An `.xz` stream writer.
///
/// Push bytes with [`XzEncoder::push`], end the stream with
/// [`XzEncoder::finish`], and take what has been produced so far with
/// [`XzEncoder::take_output`]. The output is produced a block at a time, so a
/// caller that drains after every push never holds more than one block's
/// compressed bytes plus the index.
pub struct XzEncoder {
    check: CheckType,
    block_size: u64,
    /// `None` only once the solid block's stream has taken it.
    lzma2: Option<Lzma2Encoder>,
    /// The LZMA2 filter's dictionary property byte.
    dict_prop: u8,
    /// The most input the solid block holds before it starts streaming: the
    /// dictionary. Up to that size the encoder is told the data size, which
    /// it uses to shrink its hash table; past it the data size makes no
    /// difference to the bytes, so they are what the buffered path writes.
    solid_threshold: u64,
    /// The solid block, once it is streaming.
    stream: Option<SolidStream>,
    /// Whether the solid block may stream at all: cleared for a caller that
    /// hands over the whole input at once, and when no thread can be started.
    may_stream: bool,
    /// Kept so that the block threads can each build their own encoder.
    props: LzmaEncProps,
    /// The non-last filters, in the order they are applied and listed, empty
    /// for a bare LZMA2 chain.
    filters: Vec<FilterFlags>,
    /// Bytes waiting to become a block.
    pending: Vec<u8>,
    /// The last block's buffer, kept to be refilled rather than reallocated.
    spare: Vec<u8>,
    /// One `(unpadded size, uncompressed size)` per block written. Spec §4.2.
    records: Vec<(u64, u64)>,
    out: Vec<u8>,
    finished: bool,
    /// How many blocks may be compressed at once. One is the default and is
    /// the path this writer took before threads existed.
    threads: usize,
    /// Whole blocks waiting for the next parallel batch, in stream order.
    #[cfg(feature = "std")]
    queue: Vec<Vec<u8>>,
    /// One LZMA2 encoder per block thread, built on first use and reused
    /// across batches — the dictionary and hash tables are the expensive part.
    #[cfg(feature = "std")]
    pool: Vec<Lzma2Encoder>,
}

impl XzEncoder {
    /// A writer for a single stream with the given LZMA2 settings.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if a setting is out of range or `lc + lp` is above 4,
    /// which LZMA2 does not allow, and [`Error::Alloc`] if the encoder state
    /// could not be allocated.
    pub fn new(props: &LzmaEncProps) -> Result<Self, Error> {
        let lzma2 = Lzma2Encoder::new(props)?;
        let dict_prop = lzma2.properties();
        let solid_threshold = u64::from(lzma2.dict_size());
        let mut enc = XzEncoder {
            check: CheckType::Crc64,
            block_size: DEFAULT_BLOCK_SIZE,
            lzma2: Some(lzma2),
            dict_prop,
            solid_threshold,
            stream: None,
            may_stream: true,
            props: *props,
            filters: Vec::new(),
            pending: Vec::new(),
            spare: Vec::new(),
            records: Vec::new(),
            out: Vec::new(),
            finished: false,
            threads: 1,
            #[cfg(feature = "std")]
            queue: Vec::new(),
            #[cfg(feature = "std")]
            pool: Vec::new(),
        };
        enc.write_stream_header();
        Ok(enc)
    }

    /// The check to put after every block. Spec §2.1.1.2; the default is
    /// CRC-64, which is what `xz` writes.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] for a check this build cannot compute — SHA-256
    /// without either crypto feature, or a reserved type.
    pub fn set_check(&mut self, check: CheckType) -> Result<(), Error> {
        if !check_supported(check) {
            return Err(Error::Param);
        }
        if self.started() {
            // The check type lives in the stream header, which is already
            // written and is repeated in the footer; changing it after bytes
            // have gone in would contradict it.
            return Err(Error::Param);
        }
        self.check = check;
        self.out.clear();
        self.write_stream_header();
        Ok(())
    }

    /// Sets the non-last filters of every block's chain, in the order they
    /// are applied: `[delta]`, `[bcj]`, `[delta, bcj]` and so on, with the
    /// LZMA2 filter appended for you.
    ///
    /// Each filter's state is reset at every block boundary, as the format
    /// requires, so a filtered stream may still be split into blocks.
    ///
    /// # Errors
    ///
    /// [`Error::Param`] if the chain is one [`FilterChain::validate`] refuses
    /// — more than three non-last filters, an LZMA2 filter among them, a
    /// filter this crate does not implement, or a misaligned BCJ start offset
    /// — or if bytes have already gone in.
    pub fn set_filters(&mut self, filters: &[FilterFlags]) -> Result<(), Error> {
        if self.started() {
            return Err(Error::Param);
        }
        if filters.len() >= MAX_FILTERS {
            return Err(Error::Param);
        }
        // Validate the chain a reader will see, which is these plus LZMA2.
        let _ = self.chain_for(filters)?;
        self.filters
            .try_reserve(filters.len())
            .map_err(|_| Error::Alloc)?;
        self.filters.clear();
        self.filters.extend_from_slice(filters);
        Ok(())
    }

    /// The non-last filters set with [`XzEncoder::set_filters`].
    #[must_use]
    pub fn filters(&self) -> &[FilterFlags] {
        &self.filters
    }

    /// The whole chain — the given filters plus this encoder's LZMA2 filter —
    /// validated the way [`crate::xz`] validates one it has just parsed.
    fn chain_for(&self, filters: &[FilterFlags]) -> Result<FilterChain, Error> {
        chain_for_props(filters, self.dict_prop)
    }

    /// Whether any input has gone in.
    fn started(&self) -> bool {
        !self.records.is_empty() || !self.pending.is_empty() || self.stream.is_some()
    }

    /// How much a single block may decode to before the writer starts
    /// another. Zero means the default (one block for everything).
    ///
    /// With the default, an input longer than the dictionary is compressed
    /// as it arrives and never held: what the writer keeps is about the
    /// encoder's own dictionary and tables, whatever the input's length. A
    /// block size of its own bounds what is held to one block per thread
    /// instead, and every block's header then declares both of its sizes.
    ///
    /// Set it before the first push. Once the single block has started
    /// streaming, a new size has no effect: the rest of the input still goes
    /// into that block. Input held before then becomes a block of its own,
    /// however large, and the new size applies to what follows it.
    pub fn set_block_size(&mut self, bytes: u64) {
        self.block_size = if bytes == 0 {
            DEFAULT_BLOCK_SIZE
        } else {
            bytes
        };
    }

    /// How many blocks may be compressed at once.
    ///
    /// `.xz` blocks are independent by construction, so this changes only how
    /// fast the stream is produced, never what it contains: at a given block
    /// size the bytes are identical at one thread and at sixteen. It has no
    /// effect while the block size is [`DEFAULT_BLOCK_SIZE`], because then
    /// there is only ever one block.
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads.max(1);
    }

    /// The thread count set with [`XzEncoder::set_threads`].
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// The bytes produced so far, which the caller now owns.
    #[must_use]
    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.out)
    }

    /// The bytes produced so far, without taking them.
    #[must_use]
    pub fn output(&self) -> &[u8] {
        &self.out
    }

    /// Adds the next uncompressed bytes, emitting blocks as they fill.
    ///
    /// # Errors
    ///
    /// Whatever the LZMA2 encoder returns, or [`Error::Alloc`].
    pub fn push(&mut self, mut data: &[u8]) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Param);
        }
        // A streaming block takes the rest of the input whatever the block
        // size says now: a size set after it started cannot split it.
        if let Some(stream) = &mut self.stream {
            return stream.push(data, &mut self.out);
        }
        if self.block_size == DEFAULT_BLOCK_SIZE && self.may_stream {
            if self.stream.is_none() {
                // Hold the input up to the dictionary; one byte past it and
                // the block starts streaming.
                let room = self
                    .solid_threshold
                    .saturating_sub(self.pending.len() as u64);
                let take = core::cmp::min(room, data.len() as u64) as usize;
                self.pending.try_reserve(take).map_err(|_| Error::Alloc)?;
                self.pending.extend_from_slice(&data[..take]);
                data = &data[take..];
                if data.is_empty() {
                    return Ok(());
                }
                self.start_stream()?;
            }
            if let Some(stream) = &mut self.stream {
                return stream.push(data, &mut self.out);
            }
            // No thread could be started: hold everything, as before.
        }
        while !data.is_empty() {
            // Input held under the default size can already be past a size
            // set since; it is then a whole block, and the take is zero.
            let room = self.block_size.saturating_sub(self.pending.len() as u64);
            let take = core::cmp::min(room, data.len() as u64) as usize;
            self.pending.try_reserve(take).map_err(|_| Error::Alloc)?;
            self.pending.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.pending.len() as u64 >= self.block_size {
                self.block_ready()?;
            }
        }
        Ok(())
    }

    /// [`XzEncoder::pending`] holds a whole block: compress it now, or queue
    /// it for the next parallel batch.
    fn block_ready(&mut self) -> Result<(), Error> {
        #[cfg(feature = "std")]
        if self.threads > 1 {
            let block = core::mem::take(&mut self.pending);
            self.queue.try_reserve(1).map_err(|_| Error::Alloc)?;
            self.queue.push(block);
            if self.queue.len() >= self.threads {
                return self.flush_queue();
            }
            return Ok(());
        }
        self.emit_block()
    }

    /// Compresses every queued block at once and appends them in order.
    ///
    /// Each block gets its own LZMA2 encoder and its own filter converters,
    /// exactly as [`XzEncoder::emit_block`] builds them, so a block's bytes do
    /// not depend on which thread produced it or on what came before.
    #[cfg(feature = "std")]
    fn flush_queue(&mut self) -> Result<(), Error> {
        if self.queue.is_empty() {
            return Ok(());
        }
        let dict_prop = self.dict_prop;
        let want = self.queue.len();
        while self.pool.len() < want {
            self.pool.try_reserve(1).map_err(|_| Error::Alloc)?;
            let enc = Lzma2Encoder::new(&self.props)?;
            self.pool.push(enc);
        }

        let (check, filters) = (self.check, &self.filters);
        let queue = core::mem::take(&mut self.queue);
        let mut done: Vec<Result<Block, Error>> = Vec::new();
        done.try_reserve_exact(want).map_err(|_| Error::Alloc)?;

        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(want);
            for (enc, data) in self.pool.iter_mut().zip(queue) {
                handles.push(
                    scope.spawn(move || compress_block(enc, check, filters, dict_prop, data)),
                );
            }
            for h in handles {
                done.push(h.join().unwrap_or(Err(Error::InternalFailure)));
            }
        });

        for block in done {
            self.append_block(block?)?;
        }
        Ok(())
    }

    /// Ends the stream: flushes the last block, then writes the index and the
    /// footer.
    ///
    /// # Errors
    ///
    /// As [`XzEncoder::push`], plus [`Error::Param`] if the stream has already
    /// been finished.
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Param);
        }
        if let Some(stream) = self.stream.take() {
            self.finish_stream(stream)?;
        }
        // Empty once a stream has started, which takes everything held; never
        // skipped on that account, so no input can be left out of the file.
        if !self.pending.is_empty() {
            self.block_ready()?;
        }
        #[cfg(feature = "std")]
        self.flush_queue()?;
        self.write_index();
        self.write_footer();
        self.finished = true;
        Ok(())
    }

    /// Spec §2.1.1: the magic, the stream flags, and a CRC-32 over the flags.
    fn write_stream_header(&mut self) {
        self.out.extend_from_slice(&XZ_MAGIC);
        let flags = self.stream_flags();
        self.out.extend_from_slice(&flags);
        self.out
            .extend_from_slice(&crate::crc::crc32(&flags).to_le_bytes());
    }

    /// Spec §2.1.1.2: a null byte, then the check id in the low four bits.
    fn stream_flags(&self) -> [u8; 2] {
        [0, self.check.id()]
    }

    /// Compresses [`XzEncoder::pending`] and appends one whole block.
    ///
    /// Spec §3: header, compressed data, padding to a multiple of four, then
    /// the check.
    fn emit_block(&mut self) -> Result<(), Error> {
        let data = core::mem::take(&mut self.pending);
        let lzma2 = self.lzma2.as_mut().ok_or(Error::InternalFailure)?;
        let block = compress_block(lzma2, self.check, &self.filters, self.dict_prop, data)?;
        // Keep the block's buffer for the next one; the filters worked in
        // place, so it is the right size already.
        self.pending = core::mem::take(&mut self.spare);
        self.pending.clear();
        self.append_block(block)
    }

    /// Starts compressing the solid block as it arrives: writes its header,
    /// hands the encoder to a [`Lzma2Pipe`], and feeds it what has been held
    /// so far.
    ///
    /// If no thread can be started the encoder comes back, nothing is
    /// written, and the block is held and compressed at the end as before.
    fn start_stream(&mut self) -> Result<(), Error> {
        let lzma2 = self.lzma2.take().ok_or(Error::InternalFailure)?;
        let pipe = match Lzma2Pipe::start(lzma2) {
            Ok(pipe) => pipe,
            Err(lzma2) => {
                self.lzma2 = Some(*lzma2);
                self.may_stream = false;
                return Ok(());
            }
        };
        let convs = if self.filters.is_empty() {
            None
        } else {
            let chain = self.chain_for(&self.filters)?;
            Some(chain.build().map_err(|_| Error::Param)?)
        };
        let header = block_header(&self.filters, self.dict_prop, None)?;
        self.out
            .try_reserve(header.len())
            .map_err(|_| Error::Alloc)?;
        self.out.extend_from_slice(&header);
        let mut stream = SolidStream {
            pipe,
            check: RunningCheck::new(self.check)?,
            convs,
            converted: Vec::new(),
            header_len: header.len(),
            uncompressed: 0,
        };
        let held = core::mem::take(&mut self.pending);
        self.spare = Vec::new();
        stream.check.update(&held);
        stream.uncompressed = held.len() as u64;
        let feed = match &mut stream.convs {
            Some(convs) => {
                let mut converted = Vec::new();
                converted
                    .try_reserve_exact(held.len())
                    .map_err(|_| Error::Alloc)?;
                convs.encode_push(&held, &mut converted);
                converted
            }
            None => held,
        };
        stream.pipe.write_owned(feed, &mut self.out)?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Ends the streaming solid block: flushes the filters, waits for the
    /// encoder, then pads, appends the check, and records the block.
    fn finish_stream(&mut self, mut stream: SolidStream) -> Result<(), Error> {
        if let Some(convs) = &mut stream.convs {
            stream.converted.clear();
            convs.encode_finish(&[], &mut stream.converted);
            stream.pipe.write(&stream.converted, &mut self.out)?;
        }
        let compressed = stream.pipe.finish(&mut self.out)?;
        // §3.2 Block Padding, over the compressed size.
        pad_to_four(&mut self.out, (compressed % 4) as usize);
        stream.check.finish(&mut self.out);
        let unpadded = stream.header_len as u64 + compressed + check_size(self.check) as u64;
        self.records.try_reserve(1).map_err(|_| Error::Alloc)?;
        self.records.push((unpadded, stream.uncompressed));
        Ok(())
    }

    /// Appends one already-compressed block and records it for the index.
    fn append_block(&mut self, block: Block) -> Result<(), Error> {
        self.out
            .try_reserve(block.unpadded + 3)
            .map_err(|_| Error::Alloc)?;
        self.out.extend_from_slice(&block.header);
        self.out.extend_from_slice(&block.compressed);
        // §3.2 Block Padding: null bytes up to a multiple of four.
        pad_to_four(&mut self.out, block.compressed.len());
        self.out.extend_from_slice(&block.check_bytes);

        self.records.try_reserve(1).map_err(|_| Error::Alloc)?;
        self.records
            .push((block.unpadded as u64, block.uncompressed));
        self.spare = block.data;
        Ok(())
    }

    /// Spec §4: the index, and the size it will make the footer declare.
    fn write_index(&mut self) {
        let start = self.out.len();
        self.out.push(0x00); // §4.1 Index Indicator.
        vli::push(self.records.len() as u64, &mut self.out);
        for &(unpadded, uncompressed) in &self.records {
            vli::push(unpadded, &mut self.out);
            vli::push(uncompressed, &mut self.out);
        }
        let body = self.out.len() - start;
        pad_to_four(&mut self.out, body);
        let crc = crate::crc::crc32(&self.out[start..]);
        self.out.extend_from_slice(&crc.to_le_bytes());
    }

    /// Spec §2.1.2: a CRC-32 over the two fields that follow it, the backward
    /// size, the stream flags, and the footer magic.
    fn write_footer(&mut self) {
        let index_size = self.index_size();
        let mut fields = [0u8; 6];
        // §2.1.2.1: the stored value is the real size in four-byte units,
        // less one.
        let backward = (index_size / 4 - 1) as u32;
        fields[..4].copy_from_slice(&backward.to_le_bytes());
        fields[4..].copy_from_slice(&self.stream_flags());
        self.out
            .extend_from_slice(&crate::crc::crc32(&fields).to_le_bytes());
        self.out.extend_from_slice(&fields);
        self.out.extend_from_slice(&XZ_FOOTER_MAGIC);
    }

    /// The index's size in bytes, padding and CRC included.
    fn index_size(&self) -> u64 {
        let mut n = 1 + vli::encoded_len(self.records.len() as u64);
        for &(unpadded, uncompressed) in &self.records {
            n += vli::encoded_len(unpadded) + vli::encoded_len(uncompressed);
        }
        (n as u64).next_multiple_of(4) + 4
    }
}

/// One compressed block, ready to be appended in stream order.
struct Block {
    header: Vec<u8>,
    compressed: Vec<u8>,
    check_bytes: Vec<u8>,
    unpadded: usize,
    uncompressed: u64,
    /// The block's own buffer, handed back so it can be reused.
    data: Vec<u8>,
}

/// The whole chain — the given filters plus an LZMA2 filter with this
/// dictionary property byte — validated the way [`crate::xz`] validates one it
/// has just parsed.
fn chain_for_props(filters: &[FilterFlags], dict_prop: u8) -> Result<FilterChain, Error> {
    let mut whole: Vec<FilterFlags> = Vec::new();
    whole
        .try_reserve(filters.len() + 1)
        .map_err(|_| Error::Alloc)?;
    whole.extend_from_slice(filters);
    let mut props = [0u8; 4];
    props[0] = dict_prop;
    whole.push(FilterFlags {
        id: FILTER_LZMA2,
        props,
        props_len: 1,
    });
    FilterChain::validate(&whole).map_err(|_| Error::Param)
}

/// Compresses one block's worth of input. Spec §3: header, compressed data,
/// then the check — the padding is put in by whoever appends it.
///
/// This is the whole of a block, and nothing in it depends on any other block,
/// which is what lets [`XzEncoder::set_threads`] run several at once.
fn compress_block(
    lzma2: &mut Lzma2Encoder,
    check: CheckType,
    filters: &[FilterFlags],
    dict_prop: u8,
    mut data: Vec<u8>,
) -> Result<Block, Error> {
    let uncompressed = data.len() as u64;

    // §3.4: the check covers the block's *uncompressed* data, which is what
    // came in, not what the filters made of it — so take it before they run.
    // They are size-preserving, so the header's uncompressed size is the same
    // either way.
    let mut check_bytes = Vec::new();
    compute_check(check, &data, &mut check_bytes)?;

    if !filters.is_empty() {
        // Fresh converters for every block: the format resets filter state at
        // each block boundary, and the reader builds them the same way.
        let chain = chain_for_props(filters, dict_prop)?;
        let mut convs = chain.build().map_err(|_| Error::Param)?;
        convs.encode_in_place(&mut data);
    }

    lzma2.set_data_size(uncompressed);
    let compressed = lzma2.encode_to_vec(&data)?;

    let header = block_header(
        filters,
        dict_prop,
        Some((compressed.len() as u64, uncompressed)),
    )?;
    let unpadded = header.len() + compressed.len() + check_size(check);
    Ok(Block {
        header,
        compressed,
        check_bytes,
        unpadded,
        uncompressed,
        data,
    })
}

/// Appends null bytes until `written` bytes are a multiple of four.
fn pad_to_four(out: &mut Vec<u8>, written: usize) {
    let pad = written.next_multiple_of(4) - written;
    out.extend(core::iter::repeat_n(0u8, pad));
}

/// Builds one block header. Spec §3.1.
///
/// `sizes` is `(compressed, uncompressed)` when they are known, which a
/// decoder is then required to check the block against; a block compressed as
/// it arrives has neither yet and leaves both out (§3.1.2), and the index
/// carries them instead. The filter chain is the given filters and then the
/// LZMA2 filter with its one dictionary property byte (§5.3.1).
fn block_header(
    filters: &[FilterFlags],
    dict_prop: u8,
    sizes: Option<(u64, u64)>,
) -> Result<Vec<u8>, Error> {
    // §3.1.2 Block Flags: filter count in bits 0-1, and the two size-present
    // bits. The filter count is stored less one, and `filters` leaves LZMA2
    // out, so it is the stored value as it stands.
    let mut flags = filters.len() as u8;
    // The size and flags bytes, then the LZMA2 filter: its id, its property
    // size, and the property byte.
    let mut body = 2 + 3;
    if let Some((compressed, uncompressed)) = sizes {
        flags |= 0x40 | 0x80;
        body += vli::encoded_len(compressed) + vli::encoded_len(uncompressed);
    }
    for f in filters {
        body += vli::encoded_len(f.id) + vli::encoded_len(f.props_len as u64) + f.props_len;
    }
    let size = body.next_multiple_of(4) + 4;
    if size > MAX_BLOCK_HEADER_SIZE {
        return Err(Error::Param);
    }

    let mut h = Vec::new();
    h.try_reserve(size).map_err(|_| Error::Alloc)?;
    // §3.1.1: the stored size is the real size in four-byte units, less one.
    h.push((size / 4 - 1) as u8);
    h.push(flags);
    if let Some((compressed, uncompressed)) = sizes {
        vli::push(compressed, &mut h);
        vli::push(uncompressed, &mut h);
    }
    // §3.1.5: the filters in the order the encoder applied them, LZMA2 last.
    for f in filters {
        vli::push(f.id, &mut h);
        vli::push(f.props_len as u64, &mut h);
        h.extend_from_slice(f.props());
    }
    vli::push(FILTER_LZMA2, &mut h);
    vli::push(1, &mut h); // §3.1.4: the size of the properties that follow.
    h.push(dict_prop);
    // §3.1.6 Header Padding, then §3.1.7 the CRC-32 over everything before it.
    h.resize(size - 4, 0);
    let crc = crate::crc::crc32(&h);
    h.extend_from_slice(&crc.to_le_bytes());
    debug_assert_eq!(h.len(), size);
    Ok(h)
}

/// Encode `src` as a whole `.xz` stream.
///
/// `check` is the per-block check, `block_size` the most one block may decode
/// to (zero for one block over the whole input).
///
/// # Errors
///
/// [`Error::Param`] if a setting is out of range or the check is one this
/// build cannot compute, [`Error::Alloc`] on allocation failure.
pub fn encode_xz(
    src: &[u8],
    props: &LzmaEncProps,
    check: CheckType,
    block_size: u64,
) -> Result<Vec<u8>, Error> {
    encode_xz_with_filters(src, props, check, block_size, &[])
}

/// Encode `src` as a whole `.xz` stream through a filter chain.
///
/// `filters` are the non-last filters in the order they are applied; the
/// LZMA2 filter is appended for you, so an empty slice is [`encode_xz`].
///
/// # Errors
///
/// As [`encode_xz`], plus [`Error::Param`] for a chain
/// [`XzEncoder::set_filters`] refuses.
pub fn encode_xz_with_filters(
    src: &[u8],
    props: &LzmaEncProps,
    check: CheckType,
    block_size: u64,
    filters: &[FilterFlags],
) -> Result<Vec<u8>, Error> {
    encode_xz_mt(src, props, check, block_size, filters, 1)
}

/// Encode `src` as a whole `.xz` stream, compressing blocks in parallel.
///
/// `threads` is how many blocks may be compressed at once; one is
/// [`encode_xz_with_filters`]. `.xz` blocks are independent, so the bytes do
/// not depend on it — only on `block_size`, which must be something other than
/// zero for there to be more than one block at all.
///
/// # Errors
///
/// As [`encode_xz_with_filters`].
pub fn encode_xz_mt(
    src: &[u8],
    props: &LzmaEncProps,
    check: CheckType,
    block_size: u64,
    filters: &[FilterFlags],
    threads: usize,
) -> Result<Vec<u8>, Error> {
    let mut enc = XzEncoder::new(props)?;
    enc.set_check(check)?;
    enc.set_filters(filters)?;
    enc.set_block_size(block_size);
    enc.set_threads(threads);
    // The whole input is in memory already, so the solid block is compressed
    // from it with its sizes known, and declares them, rather than streamed.
    enc.may_stream = false;
    enc.push(src)?;
    enc.finish()?;
    Ok(enc.take_output())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Threading is a scheduling choice, not a format one: at a given block
    /// size the stream is byte for byte what one thread writes — with a filter
    /// chain in front of LZMA2 as much as without one.
    #[test]
    #[cfg(feature = "std")]
    fn threads_do_not_change_the_xz_bytes() {
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let mut x = 0x9e37_79b9u32;
        let src: Vec<u8> = (0..400_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let delta = FilterFlags {
            id: crate::xz::filter::FILTER_DELTA,
            props: [3, 0, 0, 0],
            props_len: 1,
        };
        let x86 = FilterFlags {
            id: 0x04,
            props: [0; 4],
            props_len: 0,
        };
        for chain in [&[][..], &[delta][..], &[x86][..], &[delta, x86][..]] {
            for &block_size in &[1u64 << 15, 1 << 16, 150_000] {
                let want =
                    encode_xz_mt(&src, &props, CheckType::Crc64, block_size, chain, 1).unwrap();
                for threads in [2usize, 3, 8] {
                    let got =
                        encode_xz_mt(&src, &props, CheckType::Crc64, block_size, chain, threads)
                            .unwrap();
                    assert_eq!(
                        got,
                        want,
                        "{} filters, block {block_size}, {threads} threads",
                        chain.len()
                    );
                }
                // And it is still a stream the reader accepts.
                let mut back = Vec::new();
                std::io::Read::read_to_end(
                    &mut crate::xz::XzReader::new(std::io::Cursor::new(&want)),
                    &mut back,
                )
                .expect("round trip");
                assert_eq!(back, src);
            }
        }
    }

    #[test]
    fn an_empty_stream_is_header_empty_index_footer() {
        let props = LzmaEncProps::new();
        let out = encode_xz(b"", &props, CheckType::Crc64, 0).expect("encode");
        // 12 header + 8 index (indicator, count, two pad, CRC) + 12 footer.
        assert_eq!(out.len(), 32);
        assert_eq!(&out[..6], &XZ_MAGIC);
        assert_eq!(&out[out.len() - 2..], &XZ_FOOTER_MAGIC);
    }

    #[test]
    fn a_block_header_is_a_multiple_of_four_and_carries_its_crc() {
        let h = block_header(&[], 20, Some((1234, 65536))).expect("header");
        assert!(h.len().is_multiple_of(4));
        assert_eq!(h[0] as usize, h.len() / 4 - 1);
        // §3.1.2: one filter, so the count bits are zero.
        assert_eq!(h[1], 0xC0);
        let crc = crate::crc::crc32(&h[..h.len() - 4]);
        assert_eq!(&h[h.len() - 4..], &crc.to_le_bytes());
    }

    #[test]
    fn a_filtered_block_header_lists_its_filters_in_order() {
        let delta = FilterFlags::new(crate::xz::filter::FILTER_DELTA, &[3]).expect("props");
        let bcj = FilterFlags::new(crate::xz::bcj::BcjKind::X86.filter_id(), &[]).expect("props");
        let h = block_header(&[delta, bcj], 20, Some((1234, 65536))).expect("header");
        assert!(h.len().is_multiple_of(4));
        // §3.1.2: three filters in the chain, so the count bits hold two.
        assert_eq!(h[1], 0xC2);
        // The header parser must read back what was written, in the order it
        // was written: delta, then BCJ, then LZMA2.
        let parsed = crate::xz::BlockHeader::parse(&h).expect("parses");
        assert_eq!(parsed.chain.converters.len(), 2);
        // `converters` is the decode order, so it is the list reversed.
        assert_eq!(parsed.chain.converters[0].id, bcj.id);
        assert_eq!(parsed.chain.converters[1].id, delta.id);
        assert_eq!(parsed.chain.dict_prop, 20);
    }

    #[test]
    fn block_size_splits_the_input() {
        let props = LzmaEncProps::new().with_dict_size(1 << 16);
        let src: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let one = encode_xz(&src, &props, CheckType::Crc32, 0).expect("one block");
        let many = encode_xz(&src, &props, CheckType::Crc32, 16 * 1024).expect("five blocks");
        assert!(many.len() > one.len(), "more blocks cost ratio");
    }
}
