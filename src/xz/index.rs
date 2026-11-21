//! The index, and the block table a seekable reader builds from it.
//!
//! Spec §4. C: `Xz_ReadIndex` / `Xz_ParseIndex` in `XzIn.c`.
//!
//! The index is what makes a multi-block stream decodable on several threads
//! without decoding anything first: every block's compressed and uncompressed
//! size is in it, so every block's file offset follows by addition. Reading it
//! costs one seek to the footer and one read of the index itself.

use alloc::vec::Vec;

use super::error::{XzError, XzErrorKind, XzResult};
use super::stream::{STREAM_FOOTER_SIZE, STREAM_HEADER_SIZE, StreamFooter, StreamHeader};
use super::vli;
use crate::crc::{Crc64Xz, crc32};

/// The largest index the spec allows: 16 GiB.
pub const MAX_INDEX_SIZE: u64 = 1 << 34;
/// The smallest a record can be: two one-byte VLIs.
const MIN_RECORD_BYTES: u64 = 2;

/// What reading the indexes holds at once, charged against the caller's
/// memory limit: the serialized index while it is held, the parsed records,
/// the stream entries, and the block tables built from them.
///
/// Each limit check alone bounded one index; a file of many streams, each
/// with an index just under the limit, held all of them. The charge runs
/// across the whole walk, and is made before each allocation.
#[derive(Debug)]
pub(crate) struct MetaBudget {
    used: u64,
    limit: u64,
}

impl MetaBudget {
    pub(crate) fn new(limit: u64) -> Self {
        MetaBudget { used: 0, limit }
    }

    /// Adds `bytes` to what is held, or refuses when that would pass the
    /// limit.
    pub(crate) fn charge(&mut self, bytes: u64) -> Result<(), XzErrorKind> {
        let needed = self.used.checked_add(bytes).unwrap_or(u64::MAX);
        if needed > self.limit {
            return Err(XzErrorKind::MemoryLimit {
                needed,
                limit: self.limit,
            });
        }
        self.used = needed;
        Ok(())
    }

    /// As [`MetaBudget::charge`], for `n` values of `T`.
    pub(crate) fn charge_items<T>(&mut self, n: usize) -> Result<(), XzErrorKind> {
        self.charge(
            (n as u64)
                .checked_mul(core::mem::size_of::<T>() as u64)
                .unwrap_or(u64::MAX),
        )
    }

    /// Gives back what [`MetaBudget::charge`] took for something dropped.
    pub(crate) fn release(&mut self, bytes: u64) {
        self.used = self.used.saturating_sub(bytes);
    }

    /// As [`MetaBudget::release`], for `n` values of `T`.
    pub(crate) fn release_items<T>(&mut self, n: usize) {
        self.release((n as u64).saturating_mul(core::mem::size_of::<T>() as u64));
    }

    /// The refusal for an allocation the allocator itself failed.
    fn refused(&self) -> XzErrorKind {
        XzErrorKind::MemoryLimit {
            needed: self.used,
            limit: self.limit,
        }
    }

    /// Makes room for `additional` more values in `v`, charging by capacity:
    /// what is about to be reserved before it is, and anything the
    /// allocator gave beyond it afterwards.
    pub(crate) fn reserve<T>(
        &mut self,
        v: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), XzErrorKind> {
        let before = v.capacity();
        let want = v
            .len()
            .checked_add(additional)
            .ok_or_else(|| self.refused())?;
        if want <= before {
            return Ok(());
        }
        self.charge_items::<T>(want - before)?;
        v.try_reserve_exact(want - v.len())
            .map_err(|_| self.refused())?;
        self.charge_items::<T>(v.capacity() - want)
    }
}

/// One index record: what the stream says a block costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XzIndexRecord {
    /// Block header + compressed data + check, without block padding.
    pub unpadded_size: u64,
    /// What the block decodes to.
    pub uncompressed_size: u64,
}

impl XzIndexRecord {
    /// The block's size with its padding, which is what the next block's
    /// offset is reached by adding.
    ///
    /// # Errors
    ///
    /// `None` on overflow, which a hostile index can ask for.
    #[must_use]
    pub fn padded_size(&self) -> Option<u64> {
        Some(self.unpadded_size.checked_add(3)? & !3)
    }
}

/// One block, located.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XzBlockEntry {
    /// Offset of the block's first header byte in the file.
    pub file_offset: u64,
    /// Offset of the block's first decoded byte in the stream's output.
    pub uncompressed_offset: u64,
    /// The record this came from.
    pub record: XzIndexRecord,
}

/// The blocks that went by, folded to a fixed size.
///
/// The index repeats, for every block, sizes the blocks themselves already
/// carried, and a decoder is required to check that the two agree (spec §4.2).
/// Holding the list would let a stream of millions of tiny blocks cost memory
/// proportional to the file; instead each record is folded into a CRC-64 as it
/// is produced, and the index's records are folded the same way as they are
/// read. Any disagreement shows up in the fold.
#[derive(Debug, Default)]
pub(crate) struct IndexFold {
    pub(crate) count: u64,
    blocks_size: u64,
    uncompressed: u64,
    digest: Crc64Xz,
}

impl IndexFold {
    pub(crate) fn push(&mut self, unpadded: u64, uncompressed: u64) -> Result<(), XzErrorKind> {
        self.count += 1;
        let padded = unpadded.checked_add(3).ok_or(XzErrorKind::SizeMismatch)? & !3;
        self.blocks_size = self
            .blocks_size
            .checked_add(padded)
            .ok_or(XzErrorKind::SizeMismatch)?;
        self.uncompressed = self
            .uncompressed
            .checked_add(uncompressed)
            .ok_or(XzErrorKind::SizeMismatch)?;
        self.digest.update(&unpadded.to_le_bytes());
        self.digest.update(&uncompressed.to_le_bytes());
        Ok(())
    }

    /// Closes the fold: count, the size of the blocks with their padding, the
    /// uncompressed size, and the digest of the records.
    pub(crate) fn finish(&mut self) -> (u64, u64, u64, u64) {
        let digest = core::mem::take(&mut self.digest).finalize();
        (self.count, self.blocks_size, self.uncompressed, digest)
    }
}

/// A parsed index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XzIndex {
    /// One record per block, in stream order.
    pub records: Vec<XzIndexRecord>,
    /// The index's own size in bytes, padding and CRC included.
    pub index_size: u64,
}

impl XzIndex {
    /// Parses an index from the bytes between the last block and the footer.
    ///
    /// `buf` must be exactly the index, including its padding and CRC-32.
    ///
    /// # Errors
    ///
    /// [`XzErrorKind::IndexMismatch`] if the indicator, record count or
    /// padding are wrong, [`XzErrorKind::HeaderCrc`] on a CRC mismatch, and
    /// the VLI errors for malformed integers. The record count is bounded by
    /// the bytes actually present before a single record is allocated.
    pub fn parse(buf: &[u8]) -> Result<Self, XzErrorKind> {
        Self::parse_charged(buf, &mut MetaBudget::new(u64::MAX))
    }

    /// As [`XzIndex::parse`], charging the record table to `budget` before
    /// it is allocated.
    pub(crate) fn parse_charged(buf: &[u8], budget: &mut MetaBudget) -> Result<Self, XzErrorKind> {
        if buf.len() < 8 || !buf.len().is_multiple_of(4) || buf.len() as u64 > MAX_INDEX_SIZE {
            return Err(XzErrorKind::IndexMismatch);
        }
        let body = &buf[..buf.len() - 4];
        let stored = u32::from_le_bytes([
            buf[buf.len() - 4],
            buf[buf.len() - 3],
            buf[buf.len() - 2],
            buf[buf.len() - 1],
        ]);
        if crc32(body) != stored {
            return Err(XzErrorKind::HeaderCrc);
        }
        if body[0] != 0x00 {
            return Err(XzErrorKind::IndexMismatch);
        }

        let mut pos = 1usize;
        let count = vli::decode_at(body, &mut pos)?;
        // Every record is at least two bytes, so a count larger than the
        // bytes left cannot be honest. This is checked before `with_capacity`
        // so that a nine-byte VLI cannot ask for a 2^63-entry allocation.
        let room = (body.len() - pos) as u64 / MIN_RECORD_BYTES;
        if count > room {
            return Err(XzErrorKind::IndexMismatch);
        }
        let count_items = usize::try_from(count).map_err(|_| XzErrorKind::IndexMismatch)?;
        budget.charge_items::<XzIndexRecord>(count_items)?;
        let mut records = Vec::new();
        records
            .try_reserve_exact(count_items)
            .map_err(|_| XzErrorKind::IndexMismatch)?;
        budget.charge_items::<XzIndexRecord>(records.capacity() - count_items)?;

        let mut total_unpadded = 0u64;
        for _ in 0..count {
            let unpadded_size = vli::decode_at(body, &mut pos)?;
            let uncompressed_size = vli::decode_at(body, &mut pos)?;
            // Spec §4.3.1: never zero, and the real minimum is five.
            if unpadded_size < 5 {
                return Err(XzErrorKind::IndexMismatch);
            }
            let rec = XzIndexRecord {
                unpadded_size,
                uncompressed_size,
            };
            total_unpadded = total_unpadded
                .checked_add(rec.padded_size().ok_or(XzErrorKind::IndexMismatch)?)
                .ok_or(XzErrorKind::IndexMismatch)?;
            records.push(rec);
        }
        let _ = total_unpadded;

        // Spec §4.4: 0-3 null bytes of padding.
        let pad = body.get(pos..).ok_or(XzErrorKind::IndexMismatch)?;
        if pad.len() > 3 || pad.iter().any(|&b| b != 0) {
            return Err(XzErrorKind::IndexMismatch);
        }

        Ok(XzIndex {
            records,
            index_size: buf.len() as u64,
        })
    }

    /// How many blocks the stream has.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.records.len()
    }

    /// The stream's total uncompressed size.
    ///
    /// # Errors
    ///
    /// `None` if the records overflow, which only a malformed index can do.
    #[must_use]
    pub fn uncompressed_size(&self) -> Option<u64> {
        self.records
            .iter()
            .try_fold(0u64, |a, r| a.checked_add(r.uncompressed_size))
    }

    /// The sum of the blocks' padded sizes: the distance from the end of the
    /// stream header to the start of the index.
    ///
    /// # Errors
    ///
    /// `None` on overflow.
    #[must_use]
    pub fn blocks_size(&self) -> Option<u64> {
        self.records
            .iter()
            .try_fold(0u64, |a, r| a.checked_add(r.padded_size()?))
    }

    /// Where every block starts, given where the stream itself starts.
    ///
    /// # Errors
    ///
    /// `None` on overflow.
    #[must_use]
    pub fn blocks(&self, stream_offset: u64) -> Option<Vec<XzBlockEntry>> {
        self.blocks_into(stream_offset, Vec::with_capacity(self.records.len()))
    }

    /// As [`XzIndex::blocks`], with the table charged to `budget` and
    /// reserved before it is filled. `Ok(None)` is the overflow `blocks`
    /// answers `None` for.
    pub(crate) fn blocks_charged(
        &self,
        stream_offset: u64,
        budget: &mut MetaBudget,
    ) -> Result<Option<Vec<XzBlockEntry>>, XzErrorKind> {
        let mut out = Vec::new();
        budget.reserve(&mut out, self.records.len())?;
        Ok(self.blocks_into(stream_offset, out))
    }

    fn blocks_into(
        &self,
        stream_offset: u64,
        mut out: Vec<XzBlockEntry>,
    ) -> Option<Vec<XzBlockEntry>> {
        let mut file_offset = stream_offset.checked_add(STREAM_HEADER_SIZE as u64)?;
        let mut uncompressed_offset = 0u64;
        for &record in &self.records {
            out.push(XzBlockEntry {
                file_offset,
                uncompressed_offset,
                record,
            });
            file_offset = file_offset.checked_add(record.padded_size()?)?;
            uncompressed_offset = uncompressed_offset.checked_add(record.uncompressed_size)?;
        }
        Some(out)
    }

    /// The largest uncompressed block in the stream, which is what a parallel
    /// decode has to be able to hold per worker.
    #[must_use]
    pub fn largest_block(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.uncompressed_size)
            .max()
            .unwrap_or(0)
    }
}

/// A stream located from its footer: where it starts, its index, its flags.
#[derive(Debug, Clone)]
pub struct XzStreamIndex {
    /// Offset of the stream's first header byte in the file.
    pub stream_offset: u64,
    /// Total size of the stream, header to footer inclusive.
    pub stream_size: u64,
    /// The stream's header, whose flags the footer's must match.
    pub header: StreamHeader,
    /// The index.
    pub index: XzIndex,
}

/// Reads the index of the stream that *ends* at `end` in `reader`, by seeking
/// to its footer first.
///
/// This is the seekable path: nothing is decoded and only the footer, the
/// index and the stream header are read.
///
/// # Errors
///
/// Any structural error in the footer, index or header, and I/O errors from
/// `reader`, which are reported as [`XzErrorKind::TruncatedInput`] when they
/// are a short read.
pub fn read_stream_index_ending_at<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    end: u64,
    memory_limit: u64,
) -> XzResult<XzStreamIndex> {
    read_stream_index_charged(reader, end, &mut MetaBudget::new(memory_limit))
}

/// As [`read_stream_index_ending_at`], with the index buffer charged to
/// `budget` while it is held and the parsed records charged for good.
fn read_stream_index_charged<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    end: u64,
    budget: &mut MetaBudget,
) -> XzResult<XzStreamIndex> {
    use std::io::SeekFrom;

    let at = |off: u64| XzError::at(XzErrorKind::TruncatedInput, 0, off);
    let footer_offset = end
        .checked_sub(STREAM_FOOTER_SIZE as u64)
        .ok_or_else(|| at(end))?;
    reader
        .seek(SeekFrom::Start(footer_offset))
        .map_err(|_| at(footer_offset))?;
    let mut footer = [0u8; STREAM_FOOTER_SIZE];
    reader
        .read_exact(&mut footer)
        .map_err(|_| at(footer_offset))?;
    let footer = StreamFooter::parse(&footer).map_err(|k| XzError::at(k, 0, footer_offset))?;

    if footer.index_size > MAX_INDEX_SIZE {
        return Err(XzError::at(
            XzErrorKind::MemoryLimit {
                needed: footer.index_size,
                limit: budget.limit,
            },
            0,
            footer_offset,
        ));
    }
    let Ok(index_len) = usize::try_from(footer.index_size) else {
        return Err(XzError::at(budget.refused(), 0, footer_offset));
    };
    budget
        .charge(footer.index_size)
        .map_err(|k| XzError::at(k, 0, footer_offset))?;
    let index_offset = footer_offset
        .checked_sub(footer.index_size)
        .ok_or_else(|| at(footer_offset))?;
    if index_offset < STREAM_HEADER_SIZE as u64 {
        return Err(XzError::at(XzErrorKind::IndexMismatch, 0, index_offset));
    }

    let mut index_buf = Vec::new();
    index_buf.try_reserve_exact(index_len).map_err(|_| {
        XzError::at(
            XzErrorKind::MemoryLimit {
                needed: footer.index_size,
                limit: budget.limit,
            },
            0,
            index_offset,
        )
    })?;
    index_buf.resize(index_len, 0u8);
    reader
        .seek(SeekFrom::Start(index_offset))
        .map_err(|_| at(index_offset))?;
    reader
        .read_exact(&mut index_buf)
        .map_err(|_| at(index_offset))?;
    let index =
        XzIndex::parse_charged(&index_buf, budget).map_err(|k| XzError::at(k, 0, index_offset))?;
    // The serialized index is not kept.
    drop(index_buf);
    budget.release(footer.index_size);

    let blocks_size = index
        .blocks_size()
        .ok_or_else(|| XzError::at(XzErrorKind::IndexMismatch, 0, index_offset))?;
    // The index must sit exactly where the blocks end, which is what ties the
    // record list to the file rather than to itself.
    let stream_offset = index_offset
        .checked_sub(blocks_size)
        .and_then(|v| v.checked_sub(STREAM_HEADER_SIZE as u64))
        .ok_or_else(|| XzError::at(XzErrorKind::IndexMismatch, 0, index_offset))?;

    reader
        .seek(SeekFrom::Start(stream_offset))
        .map_err(|_| at(stream_offset))?;
    let mut head = [0u8; STREAM_HEADER_SIZE];
    reader
        .read_exact(&mut head)
        .map_err(|_| at(stream_offset))?;
    let header = StreamHeader::parse(&head).map_err(|k| XzError::at(k, 0, stream_offset))?;
    if header.flags.raw != footer.flags.raw {
        return Err(XzError::at(
            XzErrorKind::StreamFlagsMismatch,
            0,
            footer_offset,
        ));
    }

    Ok(XzStreamIndex {
        stream_offset,
        stream_size: end - stream_offset,
        header,
        index,
    })
}

/// Reads the index of every stream in the file, last stream first, and
/// returns them in file order.
///
/// This is the whole structure of a seekable `.xz` file without decoding a
/// byte of it: one seek and one small read per stream. A caller deciding
/// *whether* to widen a decode - and where its threads would cut - can ask
/// this first and commit afterwards.
///
/// The reader's position is left at the end of the file.
///
/// C: `Xz_ReadBackward` in `XzIn.c`, which does exactly this walk: strip
/// stream padding, read the footer, read the index, step to the start of the
/// stream, repeat.
///
/// # Errors
///
/// Any structural error in a footer, index or header, and short reads as
/// [`XzErrorKind::TruncatedInput`]. Bytes after the last stream that are not
/// stream padding are [`XzErrorKind::TrailingGarbage`].
pub fn stream_table<R: std::io::Read + std::io::Seek>(
    src: &mut R,
    memory_limit: u64,
) -> XzResult<Vec<XzStreamIndex>> {
    stream_table_charged(src, &mut MetaBudget::new(memory_limit))
}

/// As [`stream_table`], with everything it holds charged to `budget`, which
/// the caller goes on charging for what it builds from the table.
pub(crate) fn stream_table_charged<R: std::io::Read + std::io::Seek>(
    src: &mut R,
    budget: &mut MetaBudget,
) -> XzResult<Vec<XzStreamIndex>> {
    use std::io::SeekFrom;

    let len = src
        .seek(SeekFrom::End(0))
        .map_err(|_| XzError::at(XzErrorKind::TruncatedInput, 0, 0))?;
    if len < (STREAM_HEADER_SIZE * 2) as u64 {
        return Err(XzError::at(XzErrorKind::TruncatedInput, 0, len));
    }
    let mut end = len;
    let mut streams: Vec<XzStreamIndex> = Vec::new();
    while end > 0 {
        // Spec §5: stream padding is a whole number of null four-byte groups.
        loop {
            if end < 4 {
                return Err(XzError::at(XzErrorKind::TrailingGarbage, 0, end));
            }
            let mut tail = [0u8; 4];
            src.seek(SeekFrom::Start(end - 4))
                .and_then(|_| src.read_exact(&mut tail))
                .map_err(|_| XzError::at(XzErrorKind::TruncatedInput, 0, end))?;
            if tail == [0, 0, 0, 0] {
                end -= 4;
            } else {
                break;
            }
        }
        let idx = read_stream_index_charged(src, end, budget)?;
        if streams.len() == streams.capacity() {
            let more = streams.capacity().max(4);
            budget
                .reserve(&mut streams, more)
                .map_err(|k| XzError::at(k, 0, end))?;
        }
        end = idx.stream_offset;
        streams.push(idx);
    }
    streams.reverse();
    Ok(streams)
}

/// Every block in the file, located, in file order.
///
/// The `.xz` answer to "where could this decode be cut, without decoding it":
/// [`stream_table`] flattened, with each entry's `uncompressed_offset` made
/// relative to the whole file rather than to its own stream, so the entries
/// describe the output a reader would produce.
///
/// # Errors
///
/// As [`stream_table`], plus [`XzErrorKind::IndexMismatch`] for an index whose
/// sizes overflow when added up.
pub fn block_table<R: std::io::Read + std::io::Seek>(
    src: &mut R,
    memory_limit: u64,
) -> XzResult<Vec<XzBlockEntry>> {
    let mut budget = MetaBudget::new(memory_limit);
    let streams = stream_table_charged(src, &mut budget)?;
    let total = streams
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.index.records.len()))
        .ok_or_else(|| XzError::at(budget.refused(), 0, 0))?;
    let mut out = Vec::new();
    budget
        .reserve(&mut out, total)
        .map_err(|k| XzError::at(k, 0, 0))?;
    let mut base = 0u64;
    for s in &streams {
        let blocks = s
            .index
            .blocks_charged(s.stream_offset, &mut budget)
            .map_err(|k| XzError::at(k, 0, s.stream_offset))?
            .ok_or_else(|| XzError::at(XzErrorKind::IndexMismatch, 0, s.stream_offset))?;
        let held = blocks.capacity();
        for mut b in blocks {
            b.uncompressed_offset = b
                .uncompressed_offset
                .checked_add(base)
                .ok_or_else(|| XzError::at(XzErrorKind::IndexMismatch, 0, s.stream_offset))?;
            out.push(b);
        }
        budget.release_items::<XzBlockEntry>(held);
        base = base
            .checked_add(s.index.uncompressed_size().unwrap_or(0))
            .ok_or_else(|| XzError::at(XzErrorKind::IndexMismatch, 0, s.stream_offset))?;
    }
    Ok(out)
}

/// How many blocks the single xz stream in `reader` has, or `None` if the
/// file is not one structurally sound stream.
///
/// This is the gate a caller uses to decide between a sequential decode and a
/// parallel one *without decoding anything*: a file that is several
/// concatenated streams, that has stream padding, or whose index does not add
/// up, answers `None` and is left to the sequential reader, which is the thing
/// that validates it properly.
///
/// The caller's stream position is not restored; do that yourself if you care.
///
/// # Errors
///
/// None: every failure is `None`. This is a structural gate, not a validator.
pub fn single_stream_block_count<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
) -> Option<usize> {
    use std::io::SeekFrom;

    let len = reader.seek(SeekFrom::End(0)).ok()?;
    let idx = read_stream_index_ending_at(reader, len, MAX_INDEX_SIZE).ok()?;
    // Exactly one stream, starting at zero, with nothing after it.
    (idx.stream_offset == 0).then_some(idx.index.block_count())
}

/// Whether `reader` holds one xz stream with more than one block, which is
/// the only shape a block-parallel decode helps with.
///
/// # Errors
///
/// None; see [`single_stream_block_count`].
pub fn is_single_stream_multi_block<R: std::io::Read + std::io::Seek>(reader: &mut R) -> bool {
    single_stream_block_count(reader).is_some_and(|n| n > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_index(records: &[(u64, u64)]) -> Vec<u8> {
        let mut body = alloc::vec![0x00u8];
        let push_vli = |body: &mut Vec<u8>, mut v: u64| {
            while v >= 0x80 {
                body.push((v as u8) | 0x80);
                v >>= 7;
            }
            body.push(v as u8);
        };
        push_vli(&mut body, records.len() as u64);
        for &(u, c) in records {
            push_vli(&mut body, u);
            push_vli(&mut body, c);
        }
        while (body.len() + 4) % 4 != 0 {
            body.push(0);
        }
        let crc = crc32(&body).to_le_bytes();
        body.extend_from_slice(&crc);
        body
    }

    #[cfg(feature = "enc")]
    fn many_streams(streams: usize, blocks: usize) -> (Vec<u8>, Vec<u8>) {
        let data: Vec<u8> = (0..blocks * 16).map(|i| (i * 7 % 251) as u8).collect();
        let props = crate::LzmaEncProps::new()
            .with_level(1)
            .with_dict_size(4096);
        let one = crate::encode_xz(&data, &props, super::super::CheckType::Crc32, 16).unwrap();
        let mut file = Vec::new();
        let mut out = Vec::new();
        for _ in 0..streams {
            file.extend_from_slice(&one);
            out.extend_from_slice(&data);
        }
        (file, out)
    }

    #[cfg(feature = "enc")]
    #[test]
    fn the_index_charge_runs_across_every_stream() {
        use std::io::Cursor;
        let (file, _) = many_streams(8, 200);
        let one = read_stream_index_ending_at(&mut Cursor::new(&file), file.len() as u64, u64::MAX)
            .unwrap();
        // Each index, and each stream's records, fit the limit alone ...
        let records = 200 * core::mem::size_of::<XzIndexRecord>() as u64;
        let limit = one.index.index_size + records + 64;
        assert!(
            read_stream_index_ending_at(&mut Cursor::new(&file), file.len() as u64, limit).is_ok()
        );
        // ... but not eight of them at once.
        for err in [
            stream_table(&mut Cursor::new(&file), limit).unwrap_err(),
            block_table(&mut Cursor::new(&file), limit).unwrap_err(),
            crate::xz::XzParallelReader::with_options(
                Cursor::new(&file),
                crate::xz::XzOptions::default()
                    .with_threads(2)
                    .with_memory_limit(limit),
            )
            .err()
            .unwrap(),
        ] {
            assert!(
                matches!(err.kind, XzErrorKind::MemoryLimit { needed, limit: l } if l == limit && needed > limit),
                "{err:?}"
            );
        }
        // One index bigger than the limit is refused as it was, at its size.
        let err = stream_table(&mut Cursor::new(&file), one.index.index_size - 1).unwrap_err();
        assert_eq!(
            err.kind,
            XzErrorKind::MemoryLimit {
                needed: one.index.index_size,
                limit: one.index.index_size - 1
            }
        );
    }

    #[cfg(feature = "enc")]
    #[test]
    fn charged_tables_are_the_tables_they_were() {
        use std::io::{Cursor, Read};
        let (file, data) = many_streams(8, 200);
        let streams = stream_table(&mut Cursor::new(&file), u64::MAX).unwrap();
        assert_eq!(streams.len(), 8);
        // The block table as it was built before: each stream's own, offset.
        let mut want = Vec::new();
        let mut base = 0;
        for s in &streams {
            for mut b in s.index.blocks(s.stream_offset).unwrap() {
                b.uncompressed_offset += base;
                want.push(b);
            }
            base += s.index.uncompressed_size().unwrap();
        }
        assert_eq!(
            block_table(&mut Cursor::new(&file), u64::MAX).unwrap(),
            want
        );
        // A limit that covers what is held still decodes.
        let mut out = Vec::new();
        crate::xz::XzParallelReader::with_options(
            Cursor::new(&file),
            crate::xz::XzOptions::default()
                .with_threads(2)
                .with_memory_limit(4 << 20),
        )
        .unwrap()
        .read_to_end(&mut out)
        .unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn parses_records_and_locates_blocks() {
        let buf = build_index(&[(100, 1000), (57, 2000)]);
        let idx = XzIndex::parse(&buf).expect("index");
        assert_eq!(idx.block_count(), 2);
        assert_eq!(idx.uncompressed_size(), Some(3000));
        // 100 rounds to 100, 57 rounds to 60.
        assert_eq!(idx.blocks_size(), Some(160));
        let blocks = idx.blocks(0).expect("blocks");
        assert_eq!(blocks[0].file_offset, 12);
        assert_eq!(blocks[1].file_offset, 112);
        assert_eq!(blocks[1].uncompressed_offset, 1000);
    }

    #[test]
    fn refuses_a_record_count_larger_than_the_bytes_present() {
        let mut buf = build_index(&[(100, 1000)]);
        // Rewrite the count as 0x7F, which cannot fit in the bytes left.
        buf[1] = 0x7F;
        let crc = crc32(&buf[..buf.len() - 4]).to_le_bytes();
        let n = buf.len();
        buf[n - 4..].copy_from_slice(&crc);
        assert_eq!(XzIndex::parse(&buf), Err(XzErrorKind::IndexMismatch));
    }

    #[test]
    fn refuses_a_zero_unpadded_size_and_a_broken_crc() {
        let buf = build_index(&[(0, 10)]);
        assert_eq!(XzIndex::parse(&buf), Err(XzErrorKind::IndexMismatch));

        let mut buf = build_index(&[(100, 1000)]);
        let n = buf.len();
        buf[n - 1] ^= 0xFF;
        assert_eq!(XzIndex::parse(&buf), Err(XzErrorKind::HeaderCrc));
    }
}
