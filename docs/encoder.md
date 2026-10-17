# Porting the LZMA encoder to Rust

## Reference

The same checkout the decoder was ported from: github.com/ip7z/7zip, commit
`0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17`, tag 26.03. All of it is public
domain (`C/` in the LZMA SDK).

| C file | Role | Rust target |
| --- | --- | --- |
| `C/LzFind.h` / `C/LzFind.c` | the window and the six match finders: `MatchFinder_Create`, `MatchFinder_Init`, `MatchFinder_CheckLimits`, `MatchFinder_SetLimits`, `MatchFinder_Normalize3`, `GetMatchesSpec1`, `SkipMatchesSpec`, `Hc_GetMatchesSpec`, and the `Bt2/Bt3/Bt4/Bt5/Hc4/Hc5` entry points | `src/enc/lz_find.rs` |
| `C/LzHash.h` | `HASH2_CALC`…`HASH5_CALC` and the `kLzHash_CrcShift_*` constants | `src/enc/consts.rs`, used in `src/enc/lz_find.rs` |
| `C/LzmaEnc.h` / `C/LzmaEnc.c` | `CLzmaEncProps` and `LzmaEncProps_Normalize`; the range encoder (`CRangeEnc`, `RangeEnc_ShiftLow`, `RC_BIT`, `RC_NORM`); the price tables (`ProbPrices`, `GET_PRICE*`, `LenPriceEnc_UpdateTables`, `FillDistancesPrices`, `FillAlignPrices`); the parser (`GetOptimum`, `GetOptimumFast`, `Backward`, `COptimal`); `LzmaEnc_CodeOneBlock`, `LzmaEnc_Encode`, `LzmaEnc_WriteProperties` | `src/enc/props.rs`, `src/enc/range_enc.rs`, `src/enc/price.rs`, `src/enc/lzma_enc.rs`, `src/enc/consts.rs` |
| `C/Lzma2Enc.h` / `C/Lzma2Enc.c` | the chunk layer: `Lzma2EncInt_InitBlock`, `Lzma2EncInt_EncodeSubblock` with its copy-chunk fallback, `CLimitedSeqInStream`, `Lzma2Enc_WriteProperties`, `LZMA2_DIC_SIZE_FROM_PROP`; and the block layer: `CLzma2EncInt` per block, `Lzma2Enc_EncodeMt1` in both its stream and its memory form, `Lzma2EncProps_Normalize`'s block-size and thread arithmetic, `Lzma2Enc_MtCallback_Code` / `_Write` | `src/enc/lzma2_enc.rs`, `src/enc/stream.rs` |
| `C/MtCoder.h` / `C/MtCoder.c` | `MtCoder_Code` and `ThreadFunc2`: the read token, the block semaphore, the free-block list and the in-order write turn | `src/enc/mt_coder.rs` |
| `C/Threads.h` / `C/Threads.c` | `CSemaphore` (`Semaphore_OptCreateInit`, `_Wait`, `_Release1`), beside the `CAutoResetEvent` the decoder already had | `src/mt/sync.rs`, `src/mt/event.rs` |
| `C/Bra.c` / `C/Bra86.c` / `C/BraIA64.c` | the branch converters in the encode direction: `z7_BranchConvSt_X86_Enc` and `z7_BranchConv_{ARM64,ARM,ARMT,PPC,SPARC,IA64,RISCV}_Enc` | `src/filters/bcj.rs`, beside the decode direction |
| `C/Delta.c` | `Delta_Encode` | `src/filters/delta.rs`, beside `Delta_Decode` |
| — | the `.xz` frame, from the format specification rather than from `C/XzEnc.c` | `src/enc/xz_enc.rs` |
| — | `std::io::Write` adapters, which the C has no equivalent of | `src/enc/write.rs` |

The rules in `AGENTS.md` and `docs/porting.md` apply here as they do to the
decoder: port, do not redesign; cite the C function name; `unsafe` only where
the C relies on a margin invariant.

## Features

The encoder is behind the `enc` feature, which is on by default and which
requires `crc`. It needs `crc` because the match finder's 256-entry byte table
is the standard reflected CRC-32 table, and `src/enc/lz_find.rs` derives it
from `crate::crc` rather than building it again from `kCrcPoly`: a one-shot
CRC-32 over the single byte `b` is `table[0xFF ^ b] ^ 0xFF000000`, so
`table[i] == crc32(&[i ^ 0xFF]) ^ 0xFF000000`. The unit test
`the_table_is_the_reflected_crc32_table` checks the derived table against the C
loop entry for entry. The hash *functions* over that table — `HASH2_CALC` and
friends, with their shift constants — are the SDK's own and are ported as they
stand; they are not a CRC of anything.

The `.xz` writer is behind `xz` as well, and its block checks come from the
same places the reader's do: CRC-32 and CRC-64/XZ from `crate::crc`
(`crc-fast`), SHA-256 from `crate::crypto` (`aws-lc-rs` behind `crypto`,
`sha2` behind `native-crypto`). A check this build cannot compute is a
parameter error rather than a stream written without it.

## The filter encoders

The `.xz` writer can put a filter chain in front of LZMA2 — up to three of the
delta and BCJ converters, which is what the format allows — and
`src/enc/xz_enc.rs` writes the matching filter flags into each block header, in
the order the filters were applied, LZMA2 last.

The converters themselves live beside their decode halves. In the C, both
directions are one function taking an `encoding` flag, with
`BR_CONVERT_VAL(v, c)` expanding to `v += c` or `v -= c`; this port keeps that
shape — one `*_conv` per converter taking the flag, with `*_decode` and
`*_encode` wrappers built by the same kind of macro the C uses. Two converters
are not a flag apart and the C says so itself:

- **IA64** masks its running `pc` differently in each direction
  (`pc &= (0x1fffff << 1) | 1` encoding, `pc |= ~((0x1fffff << 1) | 1)`
  decoding), and mutates it in place, so the mutation persists across
  iterations. The port does the same.
- **RISC-V** is written out as two whole functions in `Bra.c`, because the JAL
  arm rebuilds the instruction from different halves in each direction and the
  AUIPC arm that converts is the other one. `riscv_encode` is the port of
  `Z7_BRANCH_CONV_ENC(RISCV)`; it is not `riscv_decode` with a sign flipped.

`Bcj::encode` keeps the same carry contract as `Bcj::decode`: it converts a
prefix and leaves the tail of a straddling instruction for the next call, so
encoding a buffer in pieces gives what encoding it whole gives. That is what
lets the writer filter a whole block in one call while the readers undo it
chunk by chunk.

## What was left out

**Borrowed single-threaded direct input.** Threaded and inline finders already
read borrowed blocks directly. Streamed block workers transfer their owned
input to the single-threaded finder too. Borrowed single-threaded input and
custom blocks larger than one GiB still use the copied window. The margin
and expected-size rules are documented in `src/enc/stream.rs`.

**Two host-dependent constants, pinned to their 64-bit values.** Both derive
from `sizeof(size_t)` in the C, which would make one input at one setting
compress to different bytes on a 32-bit host:

- `kNumLogBits` in `LzmaEnc.c` is `(11 + sizeof(size_t) / 8 * 3)`, pinned to
  `11 + 3 = 14`;
- `LzmaEncProps_Normalize`'s default dictionary size is
  `level <= 4 ? 1 << (level * 2 + 16) : level <= sizeof(size_t) / 2 + 4 ? 1 << (level + 20) : 1 << 28`,
  pinned at the `sizeof(size_t) == 8` branch, so level 5 is `1 << 25`.

This crate is 64-bit-first, and a compressor whose output depends on the width
of the host's pointers is not one anybody wants.

**The progress callback.** `ICompressProgress` is not carried; there is no
caller of it in this crate.

## The multi-threaded encoder

LZMA2 has exactly one seam a stream can be cut along: a *block*, a run of
chunks that opens by resetting the dictionary and therefore decodes without
reference to anything before it. `C/Lzma2Enc.c` splits the input at that seam
and hands the pieces to `C/MtCoder.c`; that is what is ported here.

| C | Rust |
| --- | --- |
| `CMtCoder`, `MtCoder_Code` | `MtCoder::code` in `src/enc/mt_coder.rs` |
| `ThreadFunc2`, the `readEvent` token passed thread to thread | `Run::thread_func2`, `Run::read_event` |
| `blocksSemaphore`, `freeBlockList`, `ReadyBlocks[]`, `writeIndex` | `Run::blocks_semaphore`, `Shared::free_block_list`, `Shared::ready_blocks`, `Shared::write_index` |
| `MTCODER_GET_NUM_BLOCKS_FROM_THREADS`, `MTCODER_BLOCKS_MAX`, the `+1` per block-size band | `num_blocks_from_threads`, `BLOCKS_MAX`, `Run::num_blocks_max` |
| `MtProgress_GetError` / `_SetError` | `Run::get_error` / `Run::set_error` |
| `SeqInStream_ReadMax` | `read_max` |
| `IMtCoderCallback2` (`Code`, `Write`) | the `MtCoderCallback` trait |
| `CLzma2EncInt`, `Lzma2EncInt_InitBlock` | `Lzma2EncInt`, `Lzma2EncInt::init_block` |
| `Lzma2Enc_EncodeMt1`, `inStream` form | `Lzma2EncInt::encode_mt1_stream` |
| `Lzma2Enc_EncodeMt1`, `inData` form | `Lzma2EncInt::encode_mt1_mem` |
| `Lzma2Enc_MtCallback_Code` / `_Write`, `me->coders[]`, `me->outBufs[]` | `Lzma2MtCallback` |
| `Lzma2EncProps_Normalize`: `blockSize` AUTO/SOLID, the `reduceSize` override, `numBlockThreads_Reduced` | `auto_block_size`, `Lzma2Encoder::block_size`, `::effective_props`, `::threads_reduced` |
| the `memUsage` reduction 7-Zip applies outside `C/` | `Lzma2Encoder::set_mem_limit`, `::mem_usage_per_thread` |
| `MatchFinder_Create`'s sizing, split out so it can be costed without allocating | `MatchFinder::plan`, `MatchFinder::mem_usage`, `LzmaEnc::mem_usage` |

`XzEncoder` needs none of `MtCoder`: `.xz` blocks are independent by
construction and the writer already queues one at a time, so
`XzEncoder::set_threads` compresses the queued blocks in a `std::thread::scope`
and appends them in order. Filter chains come along unchanged — the format
resets filter state at every block boundary, so each block builds its own
converters exactly as the single-threaded path does.

**The bytes do not depend on the thread count.** For one `(props, block size,
data size)` this encoder produces one stream, at one thread and at sixteen.
That is not an accident of scheduling: a block is encoded from its own bytes
alone, and the only thing the thread count touches is which core ran it.
`Lzma2Enc_Encode2` is the same. It is checked three ways — a unit test, a
property in the fuzz target, and `tests/lzma2_mt_parity.rs` against the C.

### The threaded match finder

`C/LzFindMt.c` and `C/LzFindOpt.c` are the *other* parallelism: not another
block, but two more threads behind a single one. `LzmaEncProps::with_num_threads(2)`
turns it on, which is the SDK's `lzmaProps.numThreads`, and
`LzmaEnc.c`'s own condition decides whether it takes effect:
`p->mtMode = (p->multiThread && !p->fastMode && (MFB.btMode != 0))`. A fast
mode level or a hash-chain match finder stays on `LzFind.c`.

Three threads share one window and one set of tables:

| thread | writes | reads |
| --- | --- | --- |
| HASH | the window below `streamPos`, the high hash, `hashBuf` block *i* | the window |
| BT | `son`, `btBuf` block *i* | the window, `hashBuf` block *i* |
| LZ (the caller) | the low hash (`hash2`/`hash3`) | the window, `btBuf` block *i* |

Nothing partitions those ranges but the SDK's own handshake, so the port keeps
it literally. `CMtSync` is a type of its own (`MtSync` in
`src/enc/lz_find_mt.rs`): two auto-reset events, two semaphores, and a critical
section the *consumer* holds across `MtSync_GetNextBlock`, because the C's
`LOCK_BUFFER` / `UNLOCK_BUFFER` pair spans a return and so cannot be an RAII
guard. The shared allocations are held as raw pointers in one `Send + Sync`
wrapper with one provenance each, and every accessor carries a SAFETY comment
naming the protocol step that makes it disjoint — `MtSync_GetNextBlock`,
`HashThreadFunc`, `BtThreadFunc`. No `&mut` is ever formed over a region
another thread can see.

| C | Rust |
| --- | --- |
| `CMtSync`, `MtSync_GetNextBlock`, `MtSync_StopWriting` | `MtSync`, `MtSync::get_next_block`, `::stop_writing` |
| `HashThreadFunc`, `BtThreadFunc`, `BtFillBlock`, `BtGetMatches` | `hash_thread_func`, `bt_thread_func`, `bt_fill_block`, `bt_get_matches` |
| `GetHeads2` … `GetHeads5b`, `USE_GetHeads_LOCAL_CRC` | `get_heads` over the `Heads` enum |
| `GetMatchesSpecN_2` (`C/LzFindOpt.c`) | `get_matches_spec_n_2` |
| `MixMatches2/3/4`, `MatchFinderMt0/2/3_Skip` | `MatchFinderMt::mix_matches`, `::skip` |
| `CMatchFinderMt`, `MatchFinderMt_Create`, `_Init`, `_GetMatches` | `MatchFinderMt`, `::create`, `::init`, `::get_matches` |
| `IMatchFinder2`, the vtable `LzmaEnc` calls through | the `Finder` enum in `src/enc/finder.rs` |
| `numTotalThreads`, the `t1`/`t2`/`t3` split in `Lzma2EncProps_Normalize` | `Lzma2Encoder::set_total_threads`, `::split_threads` |

Ownership determines the input and thread lifetimes:

- **Direct-block finder threads persist across blocks.** `run_block` borrows
  a block until both producers stop using it. Borrowed streaming input uses
  scoped threads, which stop and join before the input borrow ends.
- **Streaming input must be `Send`.** The hash thread reads it, so
  `LzmaEncoder::encode_send` / `encode_sized_send` and
  `Lzma2Encoder::encode_send` are the streaming entry points that can thread.
  Memory input can use retained producers. `encode` / `encode_sized` accept
  a plain `&mut dyn SeqInStream`; they run the finder on the coding thread,
  using the inline SDK tree kernel where applicable.

**Unlike the block thread count, this setting changes the bytes** — and that is
the C's behaviour, not this port's. `Bt5_MatchFinder_GetMatches` extends its
hash match past `numHashBytes` with `UPDATE_maxLen` and passes that length into
the binary tree, while `MixMatches4` stops at 4 and the bt thread's
`GetMatchesSpecN_2` always starts from `numHashBytes - 1`, so the two builds
of the SDK can pick different matches. `tests/lzma_parity.rs` pins this port to
the threaded C, not to the single-threaded one.

### What is deliberately different

- **Threads live for one call.** The C parks its worker threads on a
  `startEvent` and reuses them across files; `MtCoder::code` runs its workers
  inside a `std::thread::scope`, which is what lets them borrow the caller's
  input and callback instead of taking ownership. The thread count, the block
  scheduling and the output are the same; only the pool's lifetime differs.
- **`MTCODER_USE_WRITE_THREAD` is not carried.** The C `#undef`s it too, so
  this is the path the C actually takes.
- **`ICompressProgress` is not carried**, as elsewhere in this port.
- **`numThreadsMax` is the reduced count.** The C switches to the `MtCoder`
  path on `numBlockThreads_Reduced` but passes `numBlockThreads_Max` to
  `MtCoder`; this port passes the reduced count to both, because the reduced
  count is the one the memory budget and the block count allow. The output does
  not depend on it.
- **Output buffers grow.** The C sizes each block's output buffer at
  `blockSize + (blockSize >> 10) + 16` and fails with `SZ_ERROR_OUTPUT_EOF` if
  a block does not fit; here they are `Vec`s, so the copy-chunk fallback's
  worst case cannot overflow one.
- **The memory estimate is this port's own.** 7-Zip computes `GetMemUsage`
  outside `C/`, in LGPL C++ that must not be copied into this crate, so
  `mem_usage_per_thread` adds up this port's own allocation sites instead: the
  match finder's window and reference tables from `MatchFinder::plan`, the
  literal probability arrays, and the block's buffers.

### The threaded stream path

`Lzma2Encoder::encode_mt` runs `MtCoder` over a `SeqInStream` rather than a
slice, as `Lzma2Enc_Encode2` does when it is given one. It does not in general
produce what the single-threaded `encode` produces for the same input, and the
C has the same gap: a block read into memory is encoded knowing its own length,
whereas the single-threaded loop tells the encoder the block size until the
stream runs out, and `expectedDataSize` sizes the match finder's hash table.
Tell the encoder the real length with `set_data_size` and the two agree, which
is what `encode_to_vec` does.

## How the port is proved

**Bit-exact parity, `tests/lzma_parity.rs` and `tests/lzma2_encoder.rs`.**
`cargo xtask lzma-util` builds the reference binaries from the pinned SDK sources into
`target/lzma-util/`: `lzma`, which is `C/Util/Lzma/LzmaUtil.c` as it ships, and
`lzma-oracle` and `lzma2-oracle`, two small props-driven harnesses the xtask
writes out — `LzmaUtil` only ever encodes at `LzmaEncProps_Init` defaults and
never calls `LzmaEnc_SetDataSize`, which is too narrow an oracle. No reference
source is committed to this repository; the xtask reads it out of the pinned
commit. The tests compare this crate's bytes with the C's, byte for byte, over
a generated corpus — empty, one byte, all zeros, random, text-like, long
repeats, and inputs either side of the dictionary size and of the LZMA2 chunk
limits — across the match finders and several levels. They skip with a message
when the binaries are absent, and fail instead when
`LZMA_TURBO_LZMA_UTIL_REQUIRE` is set, as CI sets it.

**Threaded match finder parity, `tests/lzma_parity.rs`.** `cargo xtask
lzma-util` also builds `lzma-oracle-mt`: the LZMA1 harness compiled *without*
`-DZ7_ST`, so `LzFindMt.c` and `LzFindOpt.c` are in it, taking `numThreads` on
the command line. Every setting the single-threaded sweep covers is run again
through `MatchFinderMt` against that binary, including two dictionaries past
`0xFFFFFF`, which is what sets `MFB.bigHash` and swaps `GetHeads4`/`GetHeads5`
for `GetHeads4b`/`GetHeads5b`. `tests/mt_match_finder.rs` carries what the
corpus is too small for: three-megabyte inputs at dictionaries that force
`MatchFinder_MoveBlock` and cyclic-buffer wrap while the threads are live.
Those inputs do not reach UInt32 position normalization. The focused
`hash_normalization_preserves_wrapped_read_ahead_and_queued_distances` test
seeds that boundary, and
`encoder_position_wrap_preserves_output_and_processed_count` checks parser
positions and accounting across it. Actual generated streams beyond 4 GiB,
with SDK byte parity and independent decoding, are recorded in
`docs/mac-fleet-audit.md`.

**Threaded parity, `tests/lzma2_mt_parity.rs`.** `cargo xtask lzma-util` also
builds `lzma2-oracle-mt`: the same props-driven LZMA2 harness compiled
*without* `-DZ7_ST`, so `MtCoder.c`, `LzFindMt.c`, `LzFindOpt.c` and `Threads.c`
are in it and `Lzma2Enc_Encode2` takes its `MtCoder` path. It takes a block
size, a block thread count and a match-finder thread count on the command line.
The test compares this crate against it over the whole corpus at four block
sizes — 16 KiB, 64 KiB, 100000 and 1 MiB, either side of the corpus's own sizes
— and at one, two and four block threads; then again with `mfThreads = 2`, so
the threaded match finder is checked under the block coder as well; then checks
that the bytes are the same at one, two, three, eight and seventeen threads,
and that a `numTotalThreads` budget splits the way the C splits it; then
decodes them back through `Lzma2Reader`. CI runs it in the `encoder-parity` job on all four
platforms.

Parity is what proves the compressed data. The `.xz` frame around it has no
such oracle, because the SDK's `XzEnc.c` is not what was ported, so it is
proved by decoding instead.

**Round trips, `tests/xz_encoder.rs`.** Every check type this build can write,
at several block sizes, over the same corpus, back through `XzReader`,
`XzParallelReader` and `XzAdaptiveDecoder` — the last one fed seven bytes at a
time for the small inputs. The `Write` adapters are checked to produce byte for
byte what the one-shot calls do.

**External decode, `tests/xz_encoder.rs`.** `xz -t` and `xz -dc` over this
crate's `.xz` output, and `xz -dc --format=lzma` over its `.lzma` output,
compared with the original bytes; `7zz t` over the same streams. Both tools
are installed, pinned and digest-checked, on all four CI platforms —
`.github/scripts/install-xz.sh` for `xz`, `cargo xtask sevenzip` for `7zz` —
and `LZMA_TURBO_XZ_REQUIRE` and `LZMA_TURBO_7ZZ_REQUIRE` make a missing tool a
failure rather than a skip, as CI sets them. `LZMA_TURBO_XZ` and
`LZMA_TURBO_7ZZ` name a binary that is not on `PATH`.

**Filter parity, `tests/filter_parity.rs`.** `cargo xtask lzma-util` also
builds `filter-oracle` from the SDK's own `Bra.c`, `Bra86.c`, `BraIA64.c` and
`Delta.c`. Every BCJ kind and several delta distances are compared with it byte
for byte, in *both* directions, at several start offsets, over the shared
corpus plus generated code-like inputs — random bytes rarely contain the
instructions a branch converter looks for, so the corpus plants each kind's
opcode at its alignment. The same test checks that feeding a converter one,
three, seven, sixteen or 4096 bytes at a time gives what feeding it the whole
buffer gives.

**Filtered round trips and external decode, `tests/xz_encoder.rs`.** Every BCJ
kind on its own, with and without a start offset, three delta distances and a
delta-then-BCJ chain, at two block sizes, back through all three readers; and
`xz -t` and `xz -dc` over one large input per chain. Chains the format forbids
— LZMA2 as a non-last filter, a misaligned BCJ start offset, four non-last
filters — are checked to be refused.

**The same bytes on every platform, `tests/golden.rs`.** Parity is per
platform: each runner compares this crate against an SDK *it just built*, so
both could drift together — this file lists two places the C's own output
depends on `sizeof(size_t)`, and nothing else would notice a third. So five
inputs at six fixed settings — LZMA1, raw LZMA2 and its property byte, a
block-parallel LZMA2 at a fixed block size checked identical at one, two and
four block threads, the threaded match finder, and a delta-then-x86 `.xz`
chain — have their SHA-256 committed in `tests/golden.manifest`, hashed with
this crate's own `crypto::Sha256`. Every platform's `test` job runs
`cargo xtask golden --check`; `--write` regenerates the manifest.

**Memory safety, CI's `memory-safety` job.** The encoder's tests — the
threaded match finder, LZMA1 parity, threaded LZMA2 parity and the `enc` unit
tests — run under Valgrind's memcheck on Linux, with `LZMA_TURBO_CORPUS_MAX`
capping the corpus so that instrumenting every instruction stays inside the
job's time. Beside that, `tests/guard_pages.rs` puts the input at the end of a
guard page and the output buffer against one, from both ends, for
`LzmaEncoder`, `Lzma2Encoder` and the threaded match finder at several
dictionaries, levels, match finders, block sizes and thread counts: a read or
write one byte outside any of them faults rather than landing in allocator
slack.

**Data races, CI's `thread-sanitizer` job.** `tests/mt_match_finder.rs`,
`tests/lzma2_mt_parity.rs` and the threaded decoder's tests under
ThreadSanitizer on x86-64 Linux, on a pinned nightly with `-Zbuild-std` so the
standard library is instrumented too. The lane was checked to fail: an
unsynchronised counter written from `hash_thread_func` and `bt_thread_func`
was reported as a data race, and removed again.

**Fuzzing, `fuzz/fuzz_targets/encode_round_trip.rs`.** Arbitrary bytes at
settings taken from the input: `.lzma` back through `LzmaReader`, raw LZMA2
back through `Lzma2Decoder`, and `.xz` back through `XzReader` — then the same
`.xz` through a filter chain the input picks, back through all three readers,
the adaptive one fed in chunks the input sizes. The same input is also encoded
at a block size and thread count the input picks, both as raw LZMA2 and as
filtered `.xz`, and asserted to be byte for byte what one thread produces at
that block size.

**Differential fuzzing, `fuzz/fuzz_targets/encode_differential.rs`.** The
parity tests fix the corpus and the settings; this fixes neither. Six bytes of
the input choose the level, match finder, `lc`/`lp`/`pb`, fast bytes,
dictionary class, match-finder threads, container, block size and block
threads, and the rest is the data; the LZMA-Alone stream, the raw LZMA2 stream
and its property byte, the delta filter and the x86 branch converter must be
the SDK's bytes exactly. The reference is `tools/sdk-encoder`, which links
`LzmaEnc.c`, `Lzma2Enc.c`, `LzFindMt.c`, `MtCoder.c`, `Bra*.c` and `Delta.c`
from the pinned checkout — each file checked against its SHA-256, and built
without `Z7_ST` so the threaded paths are in — into the fuzz binary, rather
than spawning the command-line oracles once per case.
