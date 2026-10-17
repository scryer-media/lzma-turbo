# Limits

Everything this crate allocates or loops over on behalf of an input is bounded
before the input is believed, and each bound is listed here with the thing it
protects against and where it is enforced. The rule the container layer works
to: *no input, however malformed, causes a panic, an unbounded allocation, or
an unbounded loop.* A malformed input produces a typed error that names the
stream, the block and the file offset.

## The xz container

| Limit | Value | Where | What it stops |
| --- | --- | --- | --- |
| Block header size | 1024 bytes | `xz::block::MAX_BLOCK_HEADER_SIZE` | The size byte is read first and bounds the read; the header is a fixed-size stack buffer, never a heap allocation. |
| Block header CRC-32 | checked before any field is used | `xz::BlockHeader::parse` | A corrupt size or filter id is rejected as corruption rather than acted on. Spec §3.1.7. |
| Filters per block | 4 | `xz::MAX_FILTERS` | The count comes from two bits of the flags byte, so it cannot exceed four; the chain validator refuses repeats and a non-last filter in the last position. |
| Filter properties | 1 byte (LZMA2, delta), 0 or 4 (BCJ) | `xz::FilterFlags::parse` | An unsupported filter id is refused *before* its property size is trusted, so a declared size never indexes anything. |
| VLI length | 9 bytes, 63 bits, shortest encoding | `xz::vli` | A VLI cannot run off the end of a header or encode a value that would overflow later arithmetic. Non-canonical encodings are refused, as the spec's own decoder does. |
| Dictionary per block | the smaller of the declared dictionary and the block's declared uncompressed size | `xz::blockdec::dict_bytes` | A one-byte block in an `xz -9` stream costs 4 KiB, not 64 MiB. The clamp is sound because xz resets the dictionary at every block. |
| Dictionary vs. caller | `XzOptions::memory_limit` | `xz::blockdec::BlockDecoder::new` | A stream that asks for more memory than the caller allows fails with `MemoryLimit { needed, limit }` before anything is allocated. |
| Output per block | `XzOptions::max_block_size`, default 64 GiB | `xz::blockdec::BlockDecoder::room` | A block header need not declare an uncompressed size; without a cap such a block could produce output forever. |
| Output per file | `XzOptions::max_unpack_bytes`, default none | `xz::XzReader::block` | A decompression bomb built from many blocks or many streams. |
| Declared sizes | compared with what was decoded | `xz::blockdec::BlockDecoder::note` | A block that decodes to more or less than its header said is an error, not a short read. |
| Index size | 16 GiB (spec) and the caller's memory limit | `xz::MAX_INDEX_SIZE`, `xz::read_stream_index_ending_at` | The seekable path allocates the index only after the footer's size field has been bounded and `try_reserve_exact` has succeeded. |
| Index records | bounded by the index's own bytes | `xz::XzIndex::parse` | A record count of 2^63 cannot be used to reserve memory: the count must fit in the remaining bytes at two bytes a record before anything is reserved. |
| Index vs. blocks (streaming) | a 24-byte running fold | `xz::index::IndexFold` | The sequential reader verifies every record against the blocks it decoded without holding a list of them, so a stream of millions of tiny blocks costs constant memory. |
| Index position | must sit exactly where the blocks end | `xz::read_stream_index_ending_at` | An index that describes blocks the file does not contain. |
| Padding | block, index and stream padding must be null and correctly sized | `xz::XzReader` | Data hidden in padding, and a stream padding length that is not a multiple of four. |
| Stream footer | flags must equal the header's, index size must match | `xz::XzReader::footer` | A truncated or spliced stream whose footer belongs to a different stream. |
| Unverifiable checks | refused unless `allow_unverifiable` | `xz::XzReader::stream_header` | A build without SHA-256 silently returning unchecked bytes for a check-type-10 stream. |
| Truncated input | every field reports `TruncatedInput` | `xz::XzReader` | A decoder that spins on a stream that stopped arriving: a read that returns zero with the field incomplete is an error, never a retry loop. |
| Bytes held while chasing | the caller's memory limit, via back-pressure | `xz::XzAdaptiveDecoder::feed` | A caller feeding faster than it drains cannot grow the decoder without bound: `feed` takes only what fits and returns how much it took. |
| Blocks in flight | the thread ceiling, and the memory limit per worker | `xz::XzAdaptiveDecoder::try_dispatch` | A file of many small finished blocks handing every one of them to a worker at once. |
| Index while chasing | re-parsed only when more input has arrived | `xz::XzAdaptiveDecoder::index` | Quadratic work re-parsing a large index on every drain while it trickles in. |
| A lost worker | reported, never waited on | `xz::XzAdaptiveDecoder::drain` | A dispatched block that no worker can return would otherwise hang the caller; it is an `InternalFailure` instead. |

## The decoder

The LZMA and LZMA2 decoders allocate a dictionary, sized by the property byte
the caller passes, and a probability table, sized by `lc` and `lp`, and
nothing else per stream. The decode loop is the C's fast loop, unchecked in
exactly the places the C is: the Rust loop and the assembly loops are both
entered only after the margin checks the C makes, and the `// SAFETY:`
comment in `lzma_dec_decode_real` lists the three invariants that keep every
read and write in bounds. `tests/guard_pages.rs` runs both loops with the
dictionary, the probability table and the input each against a no-access
page, so a one-byte overrun faults, and the fuzz targets run them on
malformed input. With `--no-default-features` the decoder builds with no
dependency at all, so no `unsafe` code but its own is present.
