# Changelog

## 0.5.0 - 2026-09-19

- `filters::bcj2`: BCJ2, 7z's four-stream branch converter, ported in both
  directions from `C/Bcj2.c` and `C/Bcj2Enc.c`. It is not an in-place
  converter like the rest of `filters` - it splits its input into a main
  stream, a stream of absolute call targets, one of jump targets and a
  range-coded flag per branch opcode, which is how a 7z folder carries it - so
  its core is slice-driven: `Bcj2Dec::decode` takes the four input slices and
  an output window and says how much of each it took, `Bcj2Enc::encode` takes
  a source slice and four output windows, and both resume exactly where they
  stopped. `decode_to_vec` and `encode_to_streams` are the whole-buffer
  convenience over them. `no_std`, no dependency, and behind the same
  `filters` feature as `bcj` and `delta`; xz has no filter id for BCJ2, so
  there is no `xz::bcj2`.
- The encoder carries the SDK's conversion conditions, not a simplification of
  them: the relative limit (`Bcj2Enc::set_relat_limit`, default `0x0f << 24`),
  the file-size limit (`set_file_size`), the starting address (`set_ip`) and
  the three finish modes, including the block-overlap check 23.00 added so
  that a `0F 8x` marker straddling a block boundary is not converted.
- Both directions are bit-exact with the SDK. `tests/bcj2_parity.rs` compares
  all four encoded streams against a `bcj2-oracle` that `cargo xtask
  lzma-util` builds from the pinned SDK's own `Bcj2.c` and `Bcj2Enc.c`, over
  the shared corpus, generated x86-like data, every length from zero to ten,
  and a real binary; it decodes the reference encoder's streams with this
  decoder and this encoder's streams with the reference decoder; and it runs
  both directions in one-byte, seven-byte and random-sized pieces per stream
  against the one-shot result.
- The BCJ and delta converters were audited line by line against the pinned
  SDK's `Bra.c`, `Bra86.c`, `BraIA64.c` and `Delta.c`. No behavioural
  divergence was found, and `tests/filter_parity.rs` now proves that where it
  would have hidden: every converter and both directions at every length from
  zero to twice its alignment plus its lookahead, at three start offsets and
  three input alignments, and the delta filter at every length up to twice its
  distance, all against the reference.
- The readers have a memory budget, checked before anything is allocated.
  `LzmaReader::new` and `Lzma2Reader::new` refuse a header or property byte
  whose rounded dictionary, probability table and input buffer would exceed
  512 MiB, `DEFAULT_MEMORY_LIMIT`; `with_memory_limit` takes the budget
  explicitly and `u64::MAX` opts out for trusted input, and `memory_required`
  says what a header would cost. `LzmaReader::with_props` keeps its
  unrestricted policy for containers that already enforce their own limit;
  `with_props_and_memory_limit` is the checked form. The `.lzma` header is
  read from the inner reader directly, so a rejected header allocates nothing,
  and the remaining-size comparison against the caller's buffer is made in 64
  bits before it is narrowed, as `CDecoder::Read` does.

## 0.4.0 - 2026-09-19

- A `filters` feature: the BCJ and delta converters on their own, as
  `filters::bcj` and `filters::delta`, `no_std` and with no dependency. They
  used to be reachable only through `xz`, which also brings the stream layer,
  the readers and `crc-fast`; a 7z reader wants the converters and none of
  that. `xz` implies `filters` and re-exports both modules at `xz::bcj` and
  `xz::delta`, so nothing that compiled against those paths changes.
  `XzErrorKind`, which the converters' constructors return, is exported at
  the crate root in every build; `xz::error::XzErrorKind` still names it.
- The match finders extend a match eight bytes at a time. `UPDATE_maxLen`,
  `GetMatchesSpec1`, `SkipMatchesSpec` and `Hc_GetMatchesSpec` in `LzFind.c`,
  and `GetMatchesSpecN_2` in `LzFindOpt.c`, all ask the same question with
  different index arithmetic - how far do the bytes here and the bytes
  `distance` behind them agree, up to a limit - and all five asked it a byte at
  a time. `enc::match_run` asks it a word at a time: the first differing byte in
  a 64-bit XOR is the one `trailing_zeros` names, once both words are read
  little-endian, so the answer is the same byte index on every host.
- It is the same answer, so it is the same stream. The encoder stays bit-exact
  with the SDK at every setting the parity tests cover, the threaded finder
  included, and `enc::match_run`'s own tests hold it against the byte loop for
  every `(distance, start, limit)` over windows whose first difference lands on
  every offset in a word and on both sides of every word boundary.
- On an i5-1240P, encoding a 39.7 MiB tree of source and executables, medians
  of seven interleaved rounds: 8.276s to 7.178s at preset 6 (+13.3%), 10.764s
  to 9.291s at preset 9 (+13.7%), and 4.937s to 4.165s at preset 6 with two
  match-finder threads (+15.6%). On input with no long matches in it the scan
  has nothing to skip and the figure is +1.7%, never negative. The extension
  loops were 21% of the profile before and the tree walk's own cache misses are
  what is left.
- The x86 branch filter finds its next candidate a word at a time. The one
  part of `Z7_BRANCH_CONV_ST(X86)` that is a search rather than a state machine
  is the run between one `E8`/`E9` and the next, and on code the filter was not
  built for it is nearly the whole cost. The two opcodes differ only in bit 0,
  so setting bit 0 of every byte maps both onto one value and nothing else onto
  it, which turns the search into a zero-byte test over a 64-bit word. The
  state machine still runs at every hit, and the three bits of mask it carries
  between calls are untouched.
- Decoding 64.0 MiB of x86 executables through the filter on an i5-1240P,
  medians of seven interleaved rounds: 0.305s to 0.297s (+2.6%). The same
  bytes with no filter in the chain move 0.00%, which is the control.
- The delta filter adds a block at a time at wide distances. The recurrence
  only reaches back `distance` bytes, so any `distance` consecutive outputs
  depend on bytes that are already final and on nothing inside their own block;
  adding a block at a time says that as two slices that cannot overlap, and the
  add vectorizes. Below 16 the block is shorter than a vector register and the
  byte loop stays. At distance 64: 0.424s to 0.411s (+3.1%). At distance 4,
  where the byte loop still runs: +0.4%.
- Both filters are byte-for-byte what the SDK's own produce, on the SDK's own
  harness, and the x86 one is held against the four-byte loop it replaced
  directly - conversions, returns and carried state, whole and in pieces.
- `kernel-ab` is a new non-default feature that puts a cached toggle in front
  of each of the three, so the measurements above are one binary with the arm
  chosen at run time rather than two builds. It is for benchmarking and nothing
  else.

- The threaded match finder. `C/LzFindMt.c` and `C/LzFindOpt.c` are ported:
  the hash thread, the bt thread and their two ring buffers, `CMtSync`'s block
  handshake, `GetMatchesSpecN_2` and the `MixMatches*` / `MatchFinderMt*_Skip`
  family. `LzmaEncProps::with_num_threads(2)` turns it on, which is what the
  SDK's `numThreads` does: a second thread behind a *single* block, on top of
  the block parallelism below. `Lzma2Encoder::set_total_threads` is the
  SDK's `numTotalThreads`, block threads times match-finder threads, split the
  way `Lzma2EncProps_Normalize` splits it.
- It is bit-exact with the C that threads. `cargo xtask lzma-util` now also
  builds `lzma-oracle-mt`, the LZMA1 harness compiled without `Z7_ST` so that
  `LzFindMt.c` is in it, and `tests/lzma_parity.rs` runs every setting it
  covers through the threaded finder against that binary, `bigHash`
  dictionaries included. `tests/lzma2_mt_parity.rs` does the same for LZMA2
  with `mfThreads = 2` at two block sizes and two block-thread counts.
- Note that the SDK's threaded match finder does **not** always produce
  the same stream as `LzFind.c`. `Bt5_MatchFinder_GetMatches` extends its hash
  match past `numHashBytes` with `UPDATE_maxLen` and hands that length to the
  binary tree, while `MixMatches4` stops at 4 and the bt thread always starts
  from `numHashBytes - 1`; the C's own two builds differ on that. So the
  reference for this lane is the SDK built without `Z7_ST`, not the
  single-threaded oracle, and `numThreads` is a setting that can change the
  bytes.
- Streaming input needs `Send` to be given to the hash thread, so
  `LzmaEncoder::encode_send` / `encode_sized_send` and
  `Lzma2Encoder::encode_send` are the streaming entry points that can thread.
  Every memory entry point (`encode_to_vec`, `encode_xz`, `XzWriter`, ...)
  already routes through them. The existing non-`Send` `encode` is unchanged
  and always uses the single-threaded finder.
- `tools/lzma-bench --encode` takes `--mf-threads`.

- Block-parallel LZMA2. `C/MtCoder.c` and the multi-threaded paths of
  `C/Lzma2Enc.c` are ported: `Lzma2Encoder::set_block_size` divides the input
  into independent blocks the way `Lzma2EncProps_Normalize` does, and
  `set_threads` compresses them at once through the `MtCoder` port, which
  hands the finished blocks to the writer in stream order whichever thread
  produced them. The bytes do not depend on the thread count: at one block
  size, one thread and sixteen produce the same stream. Solid, single-threaded
  output stays the default, so nothing changes for existing callers.
  `XzEncoder::set_threads`, `XzWriter::set_threads` and `encode_xz_mt` do the
  same for `.xz`, where the blocks are already independent, filter chains
  included.
- `Lzma2Encoder::set_mem_limit` reduces the block-thread count until the
  estimate fits the budget, the way 7-Zip reduces `numBlockThreads_Reduced`
  for `memUsage`; the estimate is this port's own allocation arithmetic.
- Proved against the C that actually threads: `cargo xtask lzma-util` now also
  builds `lzma2-oracle-mt`, the same props-driven LZMA2 harness compiled
  without `Z7_ST` so `MtCoder.c`, `LzFindMt.c` and `Threads.c` are in it, and
  `tests/lzma2_mt_parity.rs` compares against it at four block sizes and three
  thread counts over the whole corpus. The encoder-parity CI job runs it on all
  four platforms, and the round-trip fuzz target now asserts thread invariance
  for raw LZMA2 and for filtered `.xz`.
- `tools/lzma-bench --encode` takes `--threads`, splitting at the block size
  `xz -T` would use and timing against `xz -T<n>` at the same preset.
- The trust stages the encoder was missing, all of them required in CI. No
  library code changed for any of it; every finding it would have caught is a
  finding it can catch now.
  - **Memory safety.** The `memory-safety` job now also runs the encoder's
    tests under Valgrind's memcheck, with `LZMA_TURBO_CORPUS_MAX` capping the
    generated corpus so instrumenting every instruction fits the job rather
    than widening its timeout. `tests/guard_pages.rs` gained encoder cases:
    the input ending exactly at a guard page and the output buffer against
    one, from both ends, for `LzmaEncoder`, `Lzma2Encoder` and the threaded
    match finder.
  - **Data races.** A new `thread-sanitizer` job runs the threaded match
    finder, the block coder and the threaded decoder under ThreadSanitizer on
    x86-64 Linux, on a pinned nightly with `-Zbuild-std`. It was checked to
    fail on a race before being trusted to pass.
  - **Fuzzing.** `cargo xtask fuzz` splits its budget across every target, so
    the workflow's timeout keeps its meaning as targets are added, and a new
    `fuzz-check` job builds every target on every change - `fuzz/` is its own
    workspace, so nothing else does.
  - **Differential fuzzing against the SDK's encoder.**
    `fuzz/fuzz_targets/encode_differential.rs` picks the input *and* the
    settings and demands byte identity with the C, single- and
    multi-threaded, for `.lzma`, raw LZMA2, the delta filter and the x86
    branch converter. `tools/sdk-encoder` links `LzmaEnc.c`, `Lzma2Enc.c`,
    `LzFindMt.c`, `MtCoder.c`, `Bra*.c` and `Delta.c` from the pinned,
    digest-checked checkout into the fuzz binary.
  - **External decoders everywhere.** A pinned `xz` and a pinned `7zz`
    (`cargo xtask sevenzip`, a digest-checked download like `cargo xtask sdk`)
    are installed on all four `encoder-parity` platforms, and
    `LZMA_TURBO_XZ_REQUIRE` / `LZMA_TURBO_7ZZ_REQUIRE` turn a missing tool
    from a skip into a failure.
  - **Cross-platform byte identity.** `tests/golden.manifest` records the
    SHA-256 - taken with this crate's own `crypto::Sha256` - of a handful of
    inputs at fixed settings, and every platform's `test` job checks its bytes
    against it with `cargo xtask golden --check`. Parity alone is per
    platform, and would not notice the C and this port drifting together.
  - **`enc` in the feature lanes.** The wasm job checks `enc` with the
    in-guest checks and with the host hash hooks, and the `test` job builds
    and tests the `enc` feature on its own.

- An encoder. `LzFind.c`, `LzmaEnc.c` and `Lzma2Enc.c` from the same pinned
  LZMA SDK checkout the decoder came from, ported function by function: the
  six match finders (hc4, hc5, bt2, bt3, bt4, bt5), the range encoder, the
  price tables, the optimal parser, and the LZMA2 chunk layer with its
  copy-chunk fallback. It is bit-exact with the reference: one input at one
  setting produces the bytes the C produces, and `tests/lzma_parity.rs` and
  `tests/lzma2_encoder.rs` check that against binaries built from the pinned
  sources by `cargo xtask lzma-util`. `docs/encoder.md` has the C-to-Rust map
  and what was left out - `LzFindMt.c`, multi-threaded `Lzma2Enc`,
  `directInput`, and two constants the C derives from `sizeof(size_t)` that
  are pinned to their 64-bit values so one input compresses to one output
  everywhere.
- Writers for the three containers: `encode_lzma_alone` for `.lzma`,
  `Lzma2Encoder` for a raw LZMA2 stream, and `XzEncoder`/`encode_xz` for
  `.xz` - one stream, one or more blocks that declare both of their sizes, a
  correct index and footer, and CRC-32, CRC-64/XZ, SHA-256 or no check under
  the same feature gates the reader uses. The `.xz` frame has no counterpart
  in the SDK, so it is proved by decoding instead of by parity: every check
  type at several block sizes goes back through `XzReader`,
  `XzParallelReader` and `XzAdaptiveDecoder`, and then through `xz -t`,
  `xz -dc` and `7zz t`.
- The BCJ and delta filters, in the encode direction. `Delta_Encode` from
  `C/Delta.c` and the eight branch converters from `C/Bra.c`, `C/Bra86.c` and
  `C/BraIA64.c` (x86, PPC, IA64, ARM, ARMT, SPARC, ARM64, RISC-V) are ported
  beside the decode halves they already had, keeping the same carry contract,
  so encoding a buffer in pieces gives what encoding it whole gives. The `.xz`
  writer takes a filter chain — `XzEncoder::set_filters`,
  `encode_xz_with_filters` — validates it the way the reader validates one it
  has parsed, and writes the matching block-header filter flags.
  `tests/filter_parity.rs` compares every converter with the SDK's own, in
  both directions, byte for byte.
- A decode bug the new writer exposed: a block with a filter chain whose last
  bytes fell inside a converter's carry could make `XzAdaptiveDecoder` report
  the stream as truncated. The block decoder returned "no input consumed, no
  output produced" — its signal for *needs more input* — for a step that had
  in fact made progress. `XzReader` and `XzParallelReader` were unaffected.
- `std::io::Write` adapters behind `std`: `LzmaWriter`, `Lzma2Writer` and
  `XzWriter`, the mirror of the `Read` adapters the decoder has. `XzWriter`
  really streams - it emits each block as it fills - which the other two
  cannot, because the size in their header is not known until the writer is
  closed.
- A new default feature, `enc`, which carries all of the above and requires
  `crc`. The match finder's 256-entry byte table is the standard reflected
  CRC-32 table, and it is now derived from `crate::crc` rather than built
  again from `kCrcPoly`; the hash functions over it are unchanged. Turning
  `enc` off builds the crate exactly as 0.3.4 did.
- `xz::vli` gained `encode` and `push`, the other half of `decode`.
- Tooling: `cargo xtask lzma-util` builds the reference encoder and two
  props-driven oracles from the pinned SDK sources, plus `filter-oracle` over
  the SDK's own `Bra.c`, `Bra86.c`, `BraIA64.c` and `Delta.c`; CI runs them on the same
  four-platform matrix the XZ Utils suite uses, with
  `LZMA_TURBO_LZMA_UTIL_REQUIRE=1` so a missing oracle fails rather than
  skips. `tools/lzma-bench` gained `--encode`, which compresses to `.xz` at
  each preset and times it against `xz -T1 -N` on the same data, reporting
  output size alongside throughput. `xtask` no longer carries a hand-written
  SHA-256 for checking fetched tarballs; it uses the `sha2` crate, with the
  pinned digests unchanged. A new fuzz target, `encode_round_trip`, drives
  arbitrary bytes through all three writers and back.

- The bulk checksums can be handed to the embedding host on wasm. Two new
  features, `crc-host` and `crypto-host`, route CRC-32, CRC-64/XZ and SHA-256
  through plain `fn` pointers the embedder installs at start-up
  (`lzma_turbo::hooks::install_host_hash_hooks`) instead of computing them in
  the guest. Both libraries those checks normally use are there for
  instructions wasm does not have - `crc-fast` for the carry-less multiply
  units, `sha2` for the SHA extensions - so a host that has them can checksum
  an `.xz` stream far faster than the guest can, and the guest hands it nothing
  but a byte range it already owns.

  The public API is unchanged: `crc::Crc32`, `crc::Crc64Xz`, `crc::crc32`,
  `crc::crc64_xz` and `crypto::Sha256` keep their types and their methods, so
  the readers, the block checks and the multi-threaded checksum planner pick
  the delegation up with no change of their own. The features engage on
  `wasm32` only - on a native target they are accepted and inert, so feature
  unification in a mixed workspace cannot turn a native build into a
  delegating one - and they add no dependency. Checksum *folding*
  (`crc32_combine`, `crc64_xz_combine`) is never delegated: it is arithmetic
  on two integers, not a pass over data.

  The CRC hooks are seeded resumes in the finalized domain and must chain,
  `crc32(crc32(0, a), b) == crc32(0, a ++ b)`, which is what lets the streaming
  digests carry a plain integer instead of a host object. SHA-256, which has no
  seeded-resume form, is delegated as a streaming state behind an opaque handle
  - init, clone, update, finalize, drop - so a multi-gigabyte block is hashed
  as it decodes and never buffered; this crate closes every handle it opens
  exactly once. A missing or contract-violating hook panics rather than falling
  back in-guest, which would return correct bytes at exactly the speed the
  embedding exists to avoid. `src/hooks.rs` documents all of it.

  `examples/wasm_xz_conformance.rs` is a complete reference embedding for a
  core wasm module, and `tests/wasm_host_conformance.rs` is the reference host
  for its ABI: it decodes an `.xz` of each check type in a `wasm32-wasip1`
  guest under `wasmtime` and requires the report - decoded bytes, their
  digests, and the verdict on a stream whose stored check was corrupted - to
  equal the native decoder's byte for byte, then proves a guest with no hooks
  installed panics with the documented message. `wasmtime` is a
  dev-dependency, target-gated off wasm, and never enters the crate's graph.

- `XzParallelReader` now accepts a stream with no blocks. An empty input is a
  well-formed `.xz` file - `xz` writes one, and `good-0-empty.xz` and its
  concatenated and padded variants are in XZ Utils' own test suite - but the
  block plan built from the index treated an empty plan as an index that could
  not be mapped, and the reader refused the file with `IndexMismatch` where the
  sequential reader and the adaptive decoder both returned end of file.
- A filter chain may use the same filter more than once. The chain validator
  refused a repeated id, on the claim that xz's decoder refuses one;
  `lzma_validate_chain` does no such thing - it asks for 1-4 filters, every
  non-last filter to be usable as non-last, the last one to be usable as last,
  and at most three size-changing filters - so `good-1-3delta-lzma2.xz`, three
  delta filters before LZMA2, decodes with xz and was refused here as a bad
  chain by every reader. The four-filter cap and the BCJ alignment check are
  unchanged.
- The parallel block workers now require the LZMA2 end marker. A block was
  accepted once it had consumed its compressed size and produced the
  uncompressed size the index promised, but the end-of-stream control byte is
  inside the compressed size and a stream that stops without it is corrupt
  however well its sizes line up. `bad-1-lzma2-11.xz` decoded through
  `XzParallelReader` and through the threaded `XzAdaptiveDecoder`; the
  sequential `BlockDecoder` had always refused it.
- `LzmaReader` now refuses an end marker that arrives before the declared
  uncompressed size. The marker closed the stream wherever it appeared, so
  `bad-too_big_size-with_eopm.lzma` decoded short and reported success instead
  of the data error liblzma's `eopm_is_valid` gives it.
- `LzmaReader` now verifies the end of a known-size stream however the input
  arrives. Reaching the declared size ended the read there and then, so
  `bad-too_small_size-without_eopm-1.lzma`, which carries another literal after
  that point, was caught when the reader held the whole file and missed when it
  was fed a byte at a time. The reader now asks the decoder to
  confirm the end, which is the range coder being finished or the next symbol
  being the end marker, as liblzma does.

## 0.3.4 - 2026-09-17

- `live_threads()` no longer undercounts. A worker was counted from its own
  first instruction, but a thread does not run the moment it is spawned, so
  between the spawn and the OS scheduling it the pool reported fewer live
  workers than it held handles for. On an idle machine the gap is invisible;
  on a loaded two-core runner eight workers read as six. The count now rises
  with the handle and falls when the worker returns, which is what the
  documented "equal to `spawned_threads()` until cancelled" promised.

## 0.3.3 - 2026-09-17

0.3.2 was tagged and never reached crates.io; this carries its change too.

- An `.xz` block that declares no uncompressed size and runs past
  `max_unpack_bytes` (or the block cap) now fails with `TooMuchOutput`. It was
  reported as corrupt LZMA data: once the allowance was used up, LZMA2 was
  asked to read the end marker, failed on the chunk that followed instead, and
  that failure was returned as it stood. A single-threaded `xz` writes such
  blocks, so a caller hitting its own limit was told its file was broken.
- The committed test vectors are now marked binary, which fixes the Windows
  test lane. Git decides text from bytes by looking for a zero byte, and one
  small vector's uncompressed source file happens to have none, so a Windows
  checkout rewrote its three line endings and the file arrived three bytes
  longer than the stream it was compressed from. Every test that compared the
  two failed, on the assembly loop and on the portable one alike. The library
  was never affected - it decodes those streams correctly on every platform -
  but the Windows lane had therefore never been green, and nothing in the
  release gate ran the tests anywhere but Linux. The release workflow now
  runs the Windows and macOS lanes as well, and publishing waits for them.

## 0.3.2 - 2026-09-17

- The `aarch64` decode loop's assembly file now purges every macro it defines.
  Its macros are named after x86 mnemonics, and one of them, `shl`, is also a
  NEON instruction. An assembler macro outlives the file that defined it, so
  under LTO it shadowed `shl` in other crates' assembly and their build failed
  with "too many positional arguments". Seen on `aarch64-unknown-linux`; the
  decoder itself is unchanged.

## 0.3.1 - 2026-09-17

The crate is published as `lzma-turbo`. It was briefly on crates.io as
`lzma-fast`, with the same contents as 0.3.0; that name is
withdrawn. Only the crate and library names changed: replace `lzma_fast` with
`lzma_turbo` in paths and `lzma-fast` with `lzma-turbo` in manifests.

## 0.3.0 - 2026-09-17

The `.xz` container, behind the new default `xz` feature. 0.2.0's entry is
kept below as it stands: that work is unreleased, but the container is a
second public surface of its own size - a module, a reader, a filter set and a
dozen error variants - and versioning it separately keeps the two stories
apart for anyone reading the history.

- `xz::XzReader`: a `Read` adapter over an `.xz` file. Reads every stream in
  the file by default, as `xz -d` does, and verifies the stream header and
  footer, every block header's CRC-32 *before* believing any field in it, the
  declared compressed and uncompressed sizes, each block's check, the block
  and stream padding, and the index - against a running fold of the blocks
  rather than a list of them, so a stream of a million blocks costs the same
  state as a stream of one. `with_memory_limit`, `with_checks`,
  `allow_unverifiable` and `single_stream` on the reader; everything else on
  `xz::XzOptions`. A reserved bit set in the stream flags or in a block
  header's flags is an error, not something to skip past: 7-Zip 22.01 accepted
  both (CVE-2022-47112 for the stream flags, CVE-2022-47111 for the block
  flags), and a decoder that does not know what a flag means cannot know it
  parsed the rest of the header correctly.
- Filters: LZMA2, delta, and the eight BCJ converters (x86, ARM, ARM-Thumb,
  ARM64, PowerPC, SPARC, IA-64, RISC-V), each with the optional four-byte
  start-offset property, in chains of up to four in the order the format
  allows. Ported from `C/Bra.c`, `C/Bra86.c`, `C/BraIA64.c` and `C/Delta.c`,
  and public as `xz::bcj` and `xz::delta` so that other container code in this
  workspace can use them directly. The chain hands each stage a bounds-checked
  slice of the caller's buffer and counts what the stage says it wrote, so the
  class of CVE-2026-14266 (7-Zip before 26.02: the XZ mixer's running count of
  filtered output outran the destination buffer, a heap overflow) is not
  expressible here.
- Checks: none, CRC-32, CRC-64/XZ and SHA-256 (the last behind `crypto` or
  `native-crypto`). A stream whose check this build cannot compute is an error
  unless the caller opts in with `allow_unverifiable`, so unchecked bytes are
  never returned silently. The check is computed by whoever decoded the block,
  through the same `ChecksumPlan` machinery the LZMA2 workers use, and the
  caller's own split points are computed in the same pass.
- `xz::XzParallelReader`: block-parallel decoding of a seekable `.xz` file,
  ported in spirit from `C/XzDecMt.c`. The index of every stream is read
  first, so every block's offset and both its sizes are known before anything
  is decoded: blocks are scheduled directly, a worker's output buffer is also
  its dictionary, and the thread count is *degraded to fit* the caller's
  memory limit rather than the decode failing halfway through.
  `memory_estimate`, `threads`, `block_count` and `uncompressed_size` report
  what it will cost and what it will produce. Concatenated streams and stream
  padding are handled by walking the file's footers backwards. The source has
  to be `Read + Seek` but not `Send`; it is only ever read on the caller's
  thread.
- `xz::XzAdaptiveDecoder`: decoding an `.xz` file that is still arriving.
  Input is fed rather than read and output is drained as `(offset, bytes)`, so
  a caller chasing a download can write what it gets by position. Each block
  decides its own mode: a block whose header declares both its sizes and whose
  bytes have all arrived goes to a worker whole, and everything else - the
  tail being written, and any block whose header declares no compressed size -
  is chased on the caller's thread as it arrives. Blocks are consumed in file
  order and the chase runs only when no worker is outstanding, so output is
  always in order. `set_threads` takes effect at the next block, and
  `in_flight_bytes` reports what is held.
- `xz::XzAdaptiveDecoder::drain_upto(limit, sink)`: as `drain`, but stops once
  the sink has been handed `limit` bytes and keeps the rest of the block it was
  in the middle of for the next call. A caller implementing `Read` over the
  decoder can hand it the caller's own buffer instead of spilling whole blocks
  into one of its own, which makes its memory a function of what is in flight
  rather than of what has been fed.
- `xz::stream_table` and `xz::block_table`: every stream, and every block,
  located from the indexes of a seekable file without decoding anything.
  `block_table` is the whole file's block list in file order with output
  offsets relative to the file, which is what a caller deciding whether to
  widen a decode wants to see before it commits. `XzParallelReader` now uses
  `stream_table` rather than its own copy of the walk.
- `xz::probe`, `xz::single_stream_block_count` and
  `xz::is_single_stream_multi_block`: structural gates over the footer and
  index that decode nothing, for a caller choosing between a sequential and a
  parallel decode.
- `xz::XzIndex` and `xz::read_stream_index_ending_at`: the index of a stream,
  parsed from its footer, with per-block file offsets and sizes.
- `xz::XzError` carries the stream, the block and the file offset of every
  failure, and converts to `std::io::Error`.
- Every allocation the container makes is bounded before it is made, and a
  block's dictionary is clamped to the block's own declared uncompressed size,
  which is usually far below the dictionary the stream declares. The limits
  are listed in `docs/security.md`.

### LZMA2, asked for by the `sevenz-turbo` fork

- `Lzma2AdaptiveDecoder` no longer decodes on the calling thread while a worker
  is outstanding, and `set_chase(false)` turns the chase off for a caller whose
  input is already on disk. The chase decoder serialises the whole decoder
  while it holds the cursor - no worker may claim a run - and that is only the
  right trade when there is nothing else in flight, which is the
  arriving-stream case it was built for. For a stream already on disk it cost
  the fork a measured 1.50x against the same decoder's own parallel path, and
  measuring it here showed why the no-worker rule alone is not enough: fed in
  pieces smaller than a run, the chase takes every run before a worker can see
  it, and the decode never threads at all (21.7 s at one thread, 23.9 s at
  eight). With chasing off the decoder waits for the rest of the run instead,
  and the curve comes back. Off is advisory, not absolute: a run too large for
  the memory limit, a run the chase has already started, a single-threaded
  decoder, and everything after `end_of_input` are still decoded inline,
  because nothing else would decode them.
- `Lzma2AdaptiveDecoder::drain_upto(limit, sink)`, as on `XzAdaptiveDecoder`
  above and for the same reason.
- `lzma_turbo::run_boundaries(source, dict_prop)`: the runs of an LZMA2 stream
  in a `Read + Seek` source, found by seeking past chunk payloads rather than
  reading them, with the source's position restored. `Lzma2RunScanner` answers
  this for bytes as they arrive; this answers it for bytes already on disk.
  `Lzma2RunScanner::payload_remaining` and `skip_payload` are the two methods
  that make skipping possible, and are public for callers driving the scanner
  over their own source.
- `Lzma2ParallelReader`'s `Read + Send + 'static` bound is documented rather
  than removed: the reader is moved onto a thread that outlives the call, so a
  scoped thread cannot serve it. `docs/porting.md` has the table of which
  driver to use for a borrowed source.

### Fixed

- A block whose decode was interrupted by an earlier block's error is no
  longer decoded at all. Its `pre_code` was skipped - which is what lends the
  coder's buffer to the decoder as its dictionary - while the code loop ran
  anyway, so the block decoded into an empty dictionary: a panic in the
  portable loop and a store through a dangling pointer in the assembly one.
  Seen as a segfault on macOS and as `STATUS_ACCESS_VIOLATION` on
  windows-msvc, on corrupt input only, and reproduced in a no-assembly build
  on both.
- A bounded `drain_upto` no longer reports a truncated LZMA2 stream as
  finished. The chase decoder can stop mid-chunk when the caller's budget runs
  out; with the end marker as the next input byte, the input cursor reached
  the end of the stream with a chunk still owing output, and that was taken
  for a clean end. The stream now ends only where the chase decoder is between
  chunks. Found by fuzzing the new drain budget.
- `Lzma2AdaptiveDecoder` fed far ahead of a stream of small runs no longer
  spends its time moving its own input. The consumed front of the input buffer
  was dropped after every dispatched run, which moves everything behind it:
  with a gigabyte fed and 1 MiB runs - what `7zz -mx1` writes for data that
  does not compress - that was most of a gigabyte moved a thousand times. It is
  now dropped once it is half the buffer, or when `feed` needs the room. A
  1 GiB archive of that shape through the `sevenz-turbo` reader at 18 threads:
  4.6 s before, 1.3 s after, against 1.1 s for `7zz t`.

Packaging: the crate is the repository root (it was `crates/lzma-turbo`), and
its archive carries the library, the README, the changelog and the license
only. The tests share a helper with the benchmark tool and read repository
fixtures, so they run from a checkout.

## 0.2.0 (unreleased)

- Multi-threaded LZMA2 decoding behind the `std` feature, ported from
  `C/Lzma2DecMt.c` and the generic `C/MtDec.c` ring of worker threads:
  `Lzma2ParallelDecoder`, `Lzma2ParallelReader`, `Lzma2MtOptions` and
  `mt_memory_estimate`. A stream with no dictionary resets, or a run larger
  than the block budget, falls back to the single-threaded decoder for that
  region without buffering it.
- `Lzma2AdaptiveDecoder`: LZMA2 decoding for a stream that is still arriving.
  Input is fed rather than read and never blocks, output is polled as
  `(offset, bytes)` blocks in order or as decoded, the thread count can be
  changed mid-stream and takes effect at the next run boundary, memory in
  flight is accounted and bounded, and the decode can be cancelled. Worker
  threads are created at the first dispatch and parked, not torn down, across
  a mode change.
- Worker-side checksums for both threaded decoders: `Checksum::{None, Crc32,
  Crc64Xz, Sha256}` with `ChecksumPlan`, `Lzma2ParallelDecoder::decode_checksummed`,
  `Lzma2ParallelReader::with_checksums` / `take_checks` / `take_segments` and
  `Lzma2AdaptiveDecoder::set_checksum` / `take_checks`. Each worker checksums
  the block it produced before it queues for the write token, so the work is
  parallel; a checksum computed by the consumer as it drains runs inside the
  ring's one serialised section instead, which measured at 16% of an
  eight-thread decode. The caller passes the absolute unpacked offsets where
  its own boundaries fall (a 7z folder's sub-streams, say) and gets one CRC
  per piece between them, in a single pass. SHA-256 is per whole block and
  ignores split points, because it cannot be folded. Behind `crc`, with
  SHA-256 additionally behind `crypto` or `native-crypto`; opting out of all
  of it leaves the decode paths exactly as they were.
- `crc::CrcFolder`, `crc::Foldable`, `crc::crc32_combine` and
  `crc::crc64_xz_combine`: fold the checksums of pieces of a stream, pushed in
  any order, into the checksum of any contiguous range they tile, without
  re-reading a byte. Available without `std`.
- Measured against 7-Zip on a gigabyte: the parallel decoder is within a few
  per cent of `7zz t -mmt=N` across the whole thread curve on both aarch64
  macOS and x86_64 Linux, and 1.6x to 12x faster than lzma-rust2's
  `Lzma2ReaderMt` at every thread count. See `docs/perf-log.md`.
- `Lzma2RunScanner` and `Lzma2Run`: incremental, public discovery of the
  independently decodable runs in an LZMA2 stream, costing O(chunks) and no
  decoding.
- `Error::CorruptRun` locates corruption by run index and output offset, and
  `Error::Cancelled` reports a cancelled decode.
- Removed `crypto::Aes256Cbc` and `crypto::sevenz_key`, and with them the
  `aes` and `cbc` dependencies. The crate is LZMA, LZMA2 and xz; 7z archives
  — the header, folders, coder graphs, BCJ and delta filters, AES-256 and the
  `7zAes.c` key derivation — are a separate crate's job, a fork of
  `sevenz-rust2` that depends on this one. `crypto` now provides SHA-256
  alone, which is what an xz stream with check type 10 needs.
- Crypto backends swapped round, and both checks turned on by default:
  `crypto` (now default) is SHA-256 over `aws-lc-rs`, and the new
  `native-crypto` is the RustCrypto `sha2` one, taking precedence when both
  are enabled so that opting out of the C build cannot be undone by another
  crate in the graph. The `aws-lc` feature name is gone. `crc` is also default
  now, because every xz stream carries a check and two of the three check
  types are CRC-32 and CRC-64/XZ. `--no-default-features` still builds as
  `no_std` + `alloc` with none of it, and nothing in `src/lzma/` or
  `src/lzma2/` can reach any of it either way.
- `Lzma2Dec_Parse` is ported as `lzma2::parse`, and the LZMA2 chunk-header
  state machine it shares with the decoder is factored out into `lzma2::frame`
  so the parser and the decoder cannot drift apart.

## 0.1.0 (unreleased)

- LZMA1 and LZMA2 decoding, ported function by function from Igor Pavlov's
  `C/LzmaDec.c` and `C/Lzma2Dec.c` (LZMA SDK 26.03, public domain): the C fast
  loop, the `LzmaDec_TryDummy` careful path, `LzmaDec_WriteRem`,
  `LzmaDec_DecodeToDic` / `DecodeToBuf`, `LzmaProps_Decode`, and the LZMA2
  chunk framing with its prop, state and dictionary resets.
- `LzmaDecoder`, `Lzma2Decoder`, `LzmaProps`, `LzmaAloneHeader`, and the
  `LzmaReader` / `Lzma2Reader` `std::io::Read` adapters behind the default
  `std` feature. The crate also builds `--no-default-features` as `no_std` +
  `alloc`.
- The `asm` feature (on by default): the hand-written decode loops from the
  SDK's `Asm/arm64/LzmaDecOpt.S` and `Asm/x86/LzmaDecOpt.asm`, translated line
  by line into Rust `naked_asm!` and used on `aarch64` and `x86_64`. Every
  other target, and `--no-default-features --features std`, get the portable
  Rust port of the C loop, which stays the differential reference for the
  assembly.
- Optional support for what a container reader around LZMA needs: `crc` gives
  CRC-32 and CRC-64/XZ from `crc-fast`, and `crypto` gives SHA-256 with a
  choice of backend. The features are additive, and with both crypto backends
  compiled a test requires them to agree. (See 0.2.0 for the backend and
  default-feature layout these ended up with.)
- No dependencies in the decoder itself, no C and no build script: the assembly is `core::arch`
  inline assembly in the crate itself. Decode only.
