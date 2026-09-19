# Porting LzmaDec.c to Rust

## Reference

- Source tree: a checkout of github.com/ip7z/7zip,
  commit `0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17`, tag 26.03.
- Files, all public domain (`C/` and `Asm/` in the LZMA SDK):

| C file | Lines | Role | Rust target |
| --- | --- | --- | --- |
| `C/LzmaDec.h` | – | state struct, props, API | `src/lzma/state.rs` |
| `C/LzmaDec.c` | 1367 | `LzmaDec_DecodeReal` (fast loop), `LzmaDec_TryDummy` (careful path), `LzmaDec_DecodeToDic`, `LzmaDec_DecodeToBuf` | `src/lzma/decode.rs`, `src/lzma/dummy.rs`, `src/lzma/mod.rs` |
| `C/Lzma2Dec.h` / `C/Lzma2Dec.c` | 493 | LZMA2 chunk framing, prop/dict resets | `src/lzma2/mod.rs` |
| `C/7zTypes.h`, `C/Compiler.h`, `C/Precomp.h` | – | macros (`kNumBitModelTotalBits`, `Z7_...`) | constants in `src/lzma/consts.rs` |
| `C/Util/Lzma/LzmaUtil.c` | – | `.lzma` header parse, streaming loop | `tools/lzma-bench` and tests |
| `C/MtDec.h` / `C/MtDec.c` | 1116 | the generic ring of worker threads: `MtDec_ThreadFunc2`, the `canRead`/`canWrite` token pair, `MtDec_Read` replay, `MTDEC_PARSE_*` | `src/mt/mtdec.rs`, `src/mt/event.rs` |
| `C/Lzma2DecMt.h` / `C/Lzma2DecMt.c` | 1000 | `CLzma2DecMtThread`, the four `Lzma2DecMt_MtCallback_*` callbacks, `Lzma2Dec_Decode_ST` | `src/mt/lzma2.rs`, `src/mt/mod.rs` |
| `C/Lzma2Dec.c` (`Lzma2Dec_Parse`) | – | walks chunk headers to find block boundaries | `src/lzma2/parse.rs`, `src/lzma2/frame.rs` |

`CPP/7zip/Compress/Lzma2Decoder.cpp` is LGPL, not public domain. It was read
only to see how `7zz` drives `Lzma2DecMt` — the block-size heuristic
(`Get_ExpectedBlockSize_From_Dict`, `kOverheadSize`, the memory-limited thread
count) is reimplemented from its behaviour in `src/mt/mod.rs`, not copied.

Not in scope: `XzDec.c`, `7zDec.c`, and the `Asm/x86/LzmaDecOpt.asm` /
`Asm/arm64/LzmaDecOpt.S` hand-written loops. The C fast loop compiled with the toolchain defaults is
the parity target for the port; the asm loop is what `7zz` ships and is the
acceptance target for the optimization phase.

## Rules

1. Faithful first. Port `LzmaDec_DecodeReal` as one function with the same
   locals (`range`, `code`, `buf`, `probs`, `dic`, `dicPos`, `processedPos`,
   `checkDicSize`, `state`, `rep0..rep3`, `limit`, `len`), the same macro
   expansions (`NORMALIZE`, `IF_BIT_0`, `UPDATE_0`, `UPDATE_1`, `TREE_DECODE`,
   `MATCHED_LITER_DEC`, `REV_BIT`), the same probability table layout and
   offsets, the same state transitions. Keep the C comments. Name things as
   the C does, in snake_case.
2. Same shape, same guarantees. `DecodeReal` runs only while
   `dicPos < limit` and `buf` has at least `LZMA_REQUIRED_INPUT_MAX` (20)
   bytes of margin. The wrapper (`LzmaDec_DecodeToDic`) enforces that and
   routes the tail through `LzmaDec_TryDummy` exactly as the C does. That
   margin is what makes unchecked loads sound; document it as the invariant
   on every `unsafe` block.
3. Unchecked where the C is unchecked, and nowhere else. Raw pointer or
   `get_unchecked` access is allowed inside the fast loop for `buf`, `probs`
   and `dic` under the margin invariant. Everything outside the loop is
   checked Rust.
4. Port, then measure, then optimize. Do not restructure before a faithful
   port passes differential tests and has a baseline number. Optimization
   means codegen: checking the assembly for bounds checks and spills, not
   redesigning the loop.
5. No `#[inline(never)]` splits of the hot loop, no trait-object dispatch in
   it, no per-bit function calls that the compiler may not inline.
6. Every step is checked against the oracle. See `docs/benchmarking.md`.

## Public API (initial)

```rust
pub struct LzmaProps { lc: u8, lp: u8, pb: u8, dict_size: u32 }
impl LzmaProps { pub fn parse(props: &[u8; 5]) -> Result<Self, Error>; }

pub struct LzmaDecoder { /* LzmaDec state + dictionary */ }
impl LzmaDecoder {
    pub fn new(props: LzmaProps) -> Result<Self, Error>;
    /// One call of LzmaDec_DecodeToBuf: consumes from `input`, writes to
    /// `output`, returns bytes read, bytes written and the status.
    pub fn decode(&mut self, input: &[u8], output: &mut [u8], finish: FinishMode)
        -> Result<Progress, Error>;
    pub fn reset(&mut self);
}

pub struct Lzma2Decoder { /* Lzma2Dec state over an LzmaDecoder */ }
impl Lzma2Decoder {
    pub fn new(dict_prop: u8) -> Result<Self, Error>;
    pub fn decode(&mut self, input: &[u8], output: &mut [u8], finish: FinishMode)
        -> Result<Progress, Error>;
}

pub enum Status { NotFinished, FinishedWithMark, NeedsMoreInput, MaybeFinishedWithoutMark }
pub enum Error { UnsupportedProps, CorruptData, /* … */ }
```

`std::io::Read` adapters (`LzmaReader`, `Lzma2Reader`) sit on top of that in
the `std` feature and are what a container parser consumes.

## Acceptance gate

Single-threaded decode wall time on `bench/fixtures/*.lzma` and the
`st.7z` pack stream, median of three, within 3% of `7zz t -mmt=1` on the
same file on the same idle machine. Until the port is at parity with the C
fast loop (`7lzma d`, no asm), the 3% number against `7zz` is not the
question; get to C parity first, then close on the asm.

## The container layer

The decoder proper is deliberately format-free: it decodes raw LZMA1 and raw
LZMA2 and knows nothing about the files those streams arrive in. The container
layer sits above it, in `src/xz/`, behind the `xz` feature, and is ported from
the same tree with the same discipline.

The container this crate covers is xz, and only xz. 7z — its header, folders
and coder graphs, its AES-256 and the `7zAes` key derivation — is out of scope
here and is handled by a fork of `sevenz-rust2` that depends on this crate.
The BCJ and delta converters are shared: they are the same filters in both
formats, so they live in `filters::bcj` and `filters::delta` behind their own
`filters` feature (`xz` implies it and re-exports them), and the 7z fork uses
them rather than carrying its own.

BCJ2 is in the same place for the same reason, even though it is 7z's alone:
xz has no filter id for it, so `filters::bcj2` is not re-exported under `xz`.
It is not an in-place converter — it splits its input into four streams, which
a 7z folder carries as four coder outputs — so its API is slice-driven rather
than the `convert in place, return how much` contract the rest of `filters`
has. Both directions are ported:

| C | Rust | Notes |
| --- | --- | --- |
| `Bcj2.c` (`Bcj2Dec_Decode`) | `src/filters/bcj2.rs` | The decoder's state machine over the four input streams, with the C's resume points: a branch target half-written into a full output window, a range coder part way through its five priming bytes. |
| `Bcj2Enc.c` (`Bcj2Enc_Encode`, `Bcj2Enc_Encode_2`) | `src/filters/bcj2.rs` | The encoder, including the `temp` lookahead `Bcj2Enc_Encode` wraps `Bcj2Enc_Encode_2` in, the three finish modes, and the relative-limit, file-size and block-overlap conditions that decide whether an offset is converted. |

Two things the xz layer needs, and which the crate already had: the checksums
in [`crate::crc`] (`crc` feature, `crc-fast`; CRC-32 is xz check type 1 and
CRC-64/XZ is type 4) and the SHA-256 in [`crate::crypto`] (check type 10),
which is `aws-lc-rs` under the `crypto` feature and RustCrypto's `sha2` under
`native-crypto`, the latter winning when both are on. `xz` therefore implies
`crc`: every xz header carries a mandatory CRC-32, so a container reader
without one could not verify anything.

### xz

C: `C/Xz.h`, `C/XzIn.c`, `C/XzDec.c`, `C/XzCrc64.c`, `C/Bra.c`, `C/Bra86.c`,
`C/BraIA64.c`, `C/Delta.c`. The file map:

| C | Rust | Notes |
| --- | --- | --- |
| `Xz.h`, `XzIn.c` (`Xz_ReadHeader`, `XzBlock_Parse`) | `src/xz/stream.rs`, `src/xz/block.rs` | Stream header and footer, block header, check types. |
| `XzIn.c` (`Xz_ReadIndex`, `Xz_ReadBackward`) | `src/xz/index.rs` | Index parsing, the seekable footer-first path, and the structural gates. |
| `Xz.c` (`Xz_ParseIndex`-adjacent VLI helpers) | `src/xz/vli.rs` | Variable-length integers, hardened: nine bytes, 63 bits, shortest encoding only. |
| `XzDec.c` (`CXzUnpacker`, `XzUnpacker_Code`) | `src/xz/reader.rs`, `src/xz/blockdec.rs` | The state machine and the per-block decode. |
| `XzDec.c` (`CXzCheck`), `XzCrc64.c` | `src/xz/check.rs`, `src/crc.rs` | The block check, computed by whoever decoded the block. |
| `Bra.c`, `Bra86.c`, `BraIA64.c` | `src/xz/bcj.rs` | All eight branch converters, decode side, with the C's branchless x86. |
| `Delta.c` | `src/xz/delta.rs` | The delta filter, with the C's 256-byte rotating history. |
| — | `src/xz/filter.rs` | Filter flags, chain validation and the streaming converter pipeline. There is no single C counterpart: 7-Zip's chain lives inside `CXzUnpacker`. |
| `XzDecMt.c` (`XzDecMt_Callback_Code`) | `src/xz/pool.rs`, `src/xz/parallel.rs` | Block-parallel decoding of a seekable file, scheduled from the index. |
| — | `src/xz/adaptive.rs` | Decoding a file that is still arriving. No C counterpart: `XzDecMt` is handed a finished file. |

A stream is a 12-byte header (magic `FD 37 7A 58 5A 00`, then two flag bytes
whose low nibble is the check type and whose CRC-32 covers the pair), a series
of blocks, an index, and a 12-byte footer. A block is a header naming one to
four filters — the last of which must be LZMA2, id `0x21`, whose one property
byte is the dictionary size — followed by the filter output, padding to a
multiple of four, and the check.

Four deliberate deviations from the C, each for a reason:

1. **The reader pulls.** `XzUnpacker_Code` is handed whole buffers; a Rust
   `Read` adapter owns its input buffer, so the state machine has to be able
   to stop in the middle of every field. That is what `src/xz/reader.rs`'s
   `State` is.
2. **The index is verified by folding, not by listing.** 7-Zip reads the whole
   index into memory. The sequential reader cannot (a hostile stream can carry
   millions of records), so it folds each block's record into a CRC-64 as the
   block goes by and folds the index's records the same way as they stream
   past. Constant memory, same verdict.
3. **The dictionary is clamped to the block.** 7-Zip allocates what the
   property byte declares. xz resets the dictionary at every block, so when a
   block header declares an uncompressed size the dictionary need never be
   larger than it — which is what lets an `xz -9` stream of small blocks
   decode under a small memory limit.

4. **A block that is still arriving is chased, not waited for.**
   `XzDecMt` is given a file and schedules it; `src/xz/adaptive.rs` is given a
   growing prefix of one. It reads each block header as it lands and decides
   per block: a header that declares both sizes says exactly where the block
   ends, so once those bytes are in the buffer the whole block goes to a
   worker without being decoded first; anything else - a header with no
   compressed size, and always the block still being written at the tail - is
   decoded on the caller's thread as the bytes arrive. Input is consumed in
   file order and the chase runs only with no worker outstanding, so the
   caller's `(offset, bytes)` output is in order by construction.

The filter chain runs in the opposite order to the header: filter 0 is the one
the encoder applied first, so the decoder undoes the list backwards, and LZMA2
(the only "last filter" this crate supports) is therefore always first to run
on the decode side. Each converter carries its own unconverted tail between
chunks — nineteen bytes in the worst chain — so a chain streams without ever
buffering a block.

### xz for weaver

The point of the container layer is that weaver can drop its `liblzma` C
dependency. These are the call sites it has today and what each becomes:

| liblzma call site | This crate |
| --- | --- |
| `xz_multistream_decoder(..., LZMA_CONCATENATED)` | `XzReader::new`, which reads every stream in the file by default. `XzOptions::with_concatenated(false)` is the single-stream form. |
| `xz_parallel_decoder` / `lzma_stream_decoder_mt` with an `lzma_mt` builder | `XzParallelReader::with_options` over a `Read + Seek` source, or `XzAdaptiveDecoder` when the file is still arriving. `XzOptions` carries what `lzma_mt` carried: `threads`, `memory_limit`, `concatenated`. |
| `memlimit` on the `lzma_mt` builder | `XzOptions::with_memory_limit`. The difference in behaviour is deliberate: liblzma fails the decode when the limit is hit, and `XzParallelReader` *degrades the thread count to fit* it instead, decoding with fewer workers rather than not at all. |
| `XZ_DECODER_MEMORY_LIMIT_BYTES` (128 MiB) | the same number, passed to `with_memory_limit`. A block's dictionary is clamped to the block's own declared uncompressed size, so `xz -9` files that liblzma would refuse under this limit decode. |
| the process-wide `XZ_MT_DECODER_PERMIT` | `XzParallelReader::memory_estimate`, which reports what a decode will cost *before* it starts, so the permit can be reserved for the real figure instead of the worst case. |
| `xz_filesystem_decoder_kind` (sequential vs parallel) | `xz::probe` and `xz::is_single_stream_multi_block`, which read the footer and index and decode nothing. |
| `xz_single_stream_block_count` | `xz::single_stream_block_count`. |
| `lzma_stream_buffer_decode` for a whole file in memory | `XzReader` over a `&[u8]`. |

Errors carry more than liblzma's `lzma_ret`: every `XzError` names the stream,
the block and the file offset it failed at, and converts to `std::io::Error`
for a call site that only wants that.

## Adaptive use

The multi-threaded decoder has a second consumer besides "decode this archive
as fast as possible": a caller decoding an archive *while it downloads*. Such
a caller stays single-threaded while it is chasing the tail of the byte flow —
low latency, low memory, decode whatever has arrived — and spreads across
threads only once a backlog of fully-arrived runs has built up, then goes back
when the backlog drains. It does this mid-stream, repeatedly.

`Lzma2DecMt`'s structure cannot serve that. In the C the threads *are* the
control flow: `MtDec_ThreadFunc2` hands two auto-reset event tokens around a
ring, thread 0 runs on the caller's stack, input is pulled from an
`ISeqInStream` that is expected to block, and the whole ring exists for the
duration of one `Lzma2DecMt_Decode` call. There is no point at which a caller
can be asked what it would like to do next.

So the port keeps the ring as the throughput path and adds a second driver
beside it. Both decode the same runs, found by the same header walk, so they
produce the same bytes:

| Path | Rust | Shape |
| --- | --- | --- |
| Throughput | `Lzma2ParallelDecoder`, `Lzma2ParallelReader` | faithful `MtDec` ring; pull from a `Read`; blocks for the whole call |
| Adaptive | `Lzma2AdaptiveDecoder` | fed input, polled output, mode switched mid-stream |
| Boundaries | `Lzma2RunScanner` | shared by both, and public |

How each constraint is met:

1. **Push/feed input, not only pull.** `Lzma2AdaptiveDecoder::feed` copies
   bytes in and returns immediately; it never decodes and never blocks.
   `drain` hands decoded blocks to a sink and returns `NeedsMoreInput`,
   `Progress` or `Finished`. Chunk headers split across feeds are carried in
   `Lzma2Frame`, which is O(1) state: nothing is ever re-scanned.
   (`tests/adaptive.rs::feeding_one_byte_at_a_time_decodes_the_same_stream`.)

2. **Run discovery is a separate, cheap, public function over bytes.**
   `Lzma2RunScanner` reads control bytes and skips payloads — O(chunks), no
   decoding — and yields `Lzma2Run { in_offset, packed_len, out_offset,
   unpacked_len, has_dict_reset }` as runs complete. `pending_runs` and
   `backlog` on the decoder are that index, so the caller sees exactly what
   the decoder sees. (`tests/scan.rs`, and
   `tests/adaptive.rs::the_backlog_of_complete_runs_is_visible`.)

   Deviation: the throughput path still uses `Lzma2Dec_Parse`
   (`src/lzma2/parse.rs`) rather than the scanner, because that parse is
   entangled with `MtDec`'s block sizing and replacing it would stop the ring
   being a port. The two cannot disagree about where a run starts: both run
   `Lzma2Frame`, the single copy of the chunk-header state machine.

3. **Mode switching at run boundaries is lossless and cheap.**
   `set_threads(n)` takes effect at the next dispatch decision, which is only
   ever made at a run boundary. A run begins with a dictionary reset, so a
   chase decoder finishing one run and a worker starting the next are
   equivalent by construction; no input is re-fed and nothing is re-decoded.
   `threads == 1` dispatches nothing, wakes nobody, and writes to the sink
   straight out of the decoder's dictionary.
   (`tests/adaptive.rs::switching_from_st_to_mt_at_any_run_boundary_is_lossless`
   covers every N; `switching_back_to_single_threaded_mid_stream_is_lossless`
   flaps in both directions.)

4. **Output is (offset, bytes), in order by default.** Every block carries its
   unpacked offset. `set_ordered(false)` releases blocks as they are decoded,
   for a sink that writes by offset. The `Read` adapter and the push `decode`
   keep in-order delivery.
   (`tests/adaptive.rs::ordered_delivery_is_the_default_and_unordered_is_opt_in`.)

5. **Workers idle cheaply and outlive a mode change.** `src/mt/pool.rs` holds
   threads that block on a channel receive and are spawned one at a time, on
   demand, at the first dispatch that needs them — a decoder built with
   sixteen threads that never dispatches creates none. A mode change changes a
   number; it does not touch the pool.
   (`tests/adaptive.rs::threads_are_created_on_first_dispatch_and_survive_a_mode_change`,
   `a_single_threaded_decoder_never_creates_a_thread`.)

6. **Memory is accounted and bounded.** `in_flight_bytes` is buffered input
   plus blocks being decoded plus blocks decoded and not yet delivered;
   dispatch is refused rather than allowed to exceed `memory_limit`, and
   `feed` takes less than it was offered for the same reason. A run too large
   to fit at all goes to the chase decoder, which streams it, rather than
   stalling. `cancel()` stops the workers and joins them before it returns.
   (`tests/adaptive.rs::a_memory_limit_bounds_what_is_in_flight`,
   `a_run_too_large_for_the_limit_is_decoded_rather_than_stalled`,
   `cancel_stops_and_joins`.)

7. **Partial-run decode while chasing.** The chase decoder is an ordinary
   `Lzma2Decoder`, which is already resumable chunk by chunk, so a run whose
   tail has not arrived still produces output. A run it has started is never
   dispatched: dispatch happens only when the cursor sits exactly on a run's
   first byte. (`tests/adaptive.rs::a_run_decodes_before_its_tail_arrives`,
   `the_threaded_path_never_claims_a_run_the_chase_started`.)

8. **Checksums are computed where the bytes are.** `Checksum::{Crc32,
   Crc64Xz, Sha256}` with `ChecksumPlan`, `set_checksum` on the adaptive
   decoder and `decode_checksummed` / `Lzma2ParallelReader::with_checksums` on
   the throughput one. Each worker checksums the block it just produced,
   before it queues for the write token; the chase decoder and the
   single-threaded fallback do the same for the runs they take, so the
   segments tile the whole output whichever path produced it.

   This is a *performance* feature, and the measurement that motivated it is
   in `docs/perf-log.md`: a table-driven CRC-32 in the benchmark's own sink
   cost 16% of an eight-thread decode, because the ring's write callback is
   its one serialised section and a checksum computed by the consumer runs
   inside it. Moving the same work into the workers is what took this crate
   from 1.10 of `7zz` to parity.

   Because a consumer's boundaries (the sub-streams of a 7z folder, say) do
   not line up with the decoder's blocks, the caller passes the absolute
   unpacked offsets where its boundaries fall and each worker emits one CRC
   per piece of its block between them, in one pass. The pieces are folded
   into whatever ranges the consumer wants with `crc::CrcFolder`, over
   `crc32_combine` / `crc64_xz_combine` — `crc-fast`'s `checksum_combine` at
   both widths, so there is no GF(2) matrix here. A worker only ever sees its
   own block, so the threaded path always cuts at block boundaries as well;
   that is a refinement of the caller's cuts and folds away.

   SHA-256 has no combine, so it is offered per whole block only and split
   points are ignored for it. That is the shape xz needs: an xz block *is* the
   unit being checked. A consumer wanting SHA-256 over an arbitrary range must
   hash that range itself, on its own thread.

   C: nothing. 7-Zip checksums a folder's sub-streams in `CFolderOutStream`,
   on the consuming thread, which is the arrangement this deliberately does
   not copy. (`tests/checksum.rs` for the whole surface;
   `tests/fixtures.rs::lzma2_mt_fixture_checksums_fold_to_the_serial_answer`
   folds a gigabyte's worth against a serial oracle.)

One further deviation, in the ring itself: `Lzma2DecMt_Decode` never reads
`p->mtc.codeRes`, so a worker's `SZ_ERROR_DATA` is silently swallowed and the
decode reports success. The port returns the error and does not write the
failing block's partial output. Output written before it stays valid, which is
what a caller writing blocks by offset needs.

## Borrowed input, and why the ring reader wants `'static`

`Lzma2ParallelReader<R: Read + Send + 'static>` is the bound consumers trip
over, and it is not an oversight. The reader is moved onto the coordinating
thread, and that thread has to outlive the call that created it: the type is
itself a `Read`, so the caller keeps it and pulls from it afterwards. A scoped
thread is exactly the thing that cannot do that - `std::thread::scope` joins
everything it spawned before it returns - so there is no cheap borrowed
variant of *this* shape to add. A borrowed one would have to invert the API
into a callback (`with_parallel_reader(src, |r| ...)`), which is a different
API, not a variant of this one.

It is also not needed, because the two other drivers already cover the
borrowing cases:

| what the caller has | what to use | the bound |
| --- | --- | --- |
| a borrowed source, output to a `Write` | `Lzma2ParallelDecoder::decode` | `R: Read + Send`, no `'static` - it owns its threads for one call and joins them before returning |
| a borrowed source, output pulled | `Lzma2AdaptiveDecoder` | none: the caller feeds slices and workers get owned copies of complete runs |
| an owned source (a file, a socket) | `Lzma2ParallelReader` | `Read + Send + 'static` |

A 7z coder's input is a bounded view of the archive's source, which is neither
`Send` nor `'static`, so it belongs in the second row - which is where the
`sevenz-turbo` fork put it, for this reason.

## The chase decoder, and when it is the wrong thing

`Lzma2AdaptiveDecoder` has two ways to decode the run at its cursor: hand the
whole run to a worker, or decode it here, chunk by chunk, as it arrives. The
second is the *chase*, and it exists so that a caller decoding a stream that is
still being written sees output before the last byte of it is written.

The chase holds the cursor while it runs, and no worker may claim a run until
it lets go, so a decoder that chases is a decoder that is not threading. Two
rules keep that from happening by accident:

1. **It stands aside for a worker.** If anything is outstanding, there is
   something to wait for, and waiting costs a fraction of a block while chasing
   costs the whole of one.
2. **It stands aside for the input, when the caller says so.**
   `set_chase(false)` says "my input is already on disk; an incomplete run at
   the cursor means I have not fed the rest of it yet, not that it does not
   exist". The decoder then returns `NeedsMoreInput` where it would have
   chased.

Rule 1 alone is not enough, and the measurement says so: fed 16 MiB at a time
from a file with eight 128 MiB runs, a decoder with rule 1 and without rule 2
never dispatches a single run, because the chase reaches every run first and
finishes it before the next feed completes it. The curve is flat - 21.7 s at
one thread, 23.9 s at eight - against 21.4 s and 8.2 s for the ring on the same
file.

Rule 2 is advisory, not absolute. Four cases decode inline whatever the caller
said, because nothing else can: a single-threaded decoder, a run the chase has
already started (the cursor is inside it), a run too large to fit the memory
limit, and anything left when `end_of_input` has been called. A decoder that
refused these would be a decoder that hangs.

C: no counterpart. `Lzma2DecMt` is given a reader and blocks on it.

## Two invariants the C keeps silently

Both of these were found the hard way, on corrupt or truncated input, and both
are the kind of thing the C source states only by the order of its statements.

**A block that was not pre-coded must not be coded.** `MtDec`'s worker skips
`PreCode` for a block interrupted by an earlier block's error, and the C skips
the code loop under the same `wasInterrupted`. `PreCode` is what lends the
coder's output buffer to the decoder as its dictionary, so coding without it
decodes into an empty dictionary: in the portable loop a slice index past the
end of a zero-length buffer, in the assembly loop a store through the dangling
pointer `Vec::new()` carries, which on aarch64 is address `0x1`. A segfault on
one platform and `STATUS_ACCESS_VIOLATION` on another, from one missing
condition.

**The end of the input is not the end of the stream.** The chase decoder can
stop wherever the caller's drain budget runs out, including in the middle of a
chunk that still owes output. If the next input byte is the LZMA2 end marker,
the input cursor is at the end of the stream while a chunk is unfinished, and
nothing about the cursor says so. A stream may only end where the decoder is
between chunks - `Lzma2Decoder::at_chunk_boundary`, the C's
`LZMA2_STATE_CONTROL` - and the completion test asks for that as well as for
the cursor. The xz adaptive decoder has this for free: a half-decoded block
leaves it in `State::Block`, and the block's padding, check and the stream's
index are all still ahead of it.
