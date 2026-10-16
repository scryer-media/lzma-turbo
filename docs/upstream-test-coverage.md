# Upstream test coverage inventory

Audited against 7-Zip SDK 26.03 at
`0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17` and XZ Utils 5.8.3.
The XZ release archive SHA-256 is
`3d3a1b973af218114f4f889bbaa2f4c037deaae0c8e815eec381c3d546b974a0`,
already pinned in `xtask/src/fetch.rs`. The local source version header,
`tests/Makefile.am`, test entry points, fixture manifest, generator and fuzz
drivers were inspected. This inventory describes coverage, not a claim that
every liblzma C API assertion has a Rust counterpart or has just been rerun.

XZ's test code and fixtures are 0BSD. The new tests acknowledge their source;
no GPL command-line helper script or LGPL 7-Zip C++ code is copied. Generated
fixtures are temporary, not Git LFS objects or new committed binary fixtures.
The existing pinned XZ decoder corpus is fetched and verified rather than
reimplemented from filenames: hand-crafted malformed inputs retain value.
The audit compared all 77 upstream `.xz`/`.lzma` filenames and SHA-256 values
with `tests/xz-utils.manifest`: no missing, additional or mismatched files.

## XZ test families

All 19 test programs/scripts registered by XZ's `tests/Makefile.am`:

| Upstream test family | Applicable coverage here | Remaining distinction |
| --- | --- | --- |
| `test_check` | `src/crc.rs` known vectors, streaming/reset/combine; `tests/checksum.rs`; new all-ID/check-size matrix | liblzma check introspection/status API differs |
| `test_hardware` | Not applicable | No public liblzma-style CPU/RAM discovery API |
| `test_stream_flags` | Stream unit tests; new exhaustive reserved flags, damaged-header bits, checks and 16 GiB backward-size boundary | C encoder/version/NULL-argument tests have no matching API |
| `test_filter_flags` | Filter unit tests; new property-length/truncation/reserved-ID/alignment matrix | C options pointers and allocated filter ownership differ |
| `test_filter_str` | Not applicable | No command-line filter-string parser/formatter API |
| `test_block_header` | Block unit tests; new optional-size combinations, 1–4 filters, >u32 sizes, reserved bits, padding and every truncation | C ABI version negotiation has no counterpart |
| `test_index` | Index unit tests; structural seek/probe tests in `xz_container`; new large totals, offsets and overflow tests | No mutable liblzma index iterator/cat/dup API; allocator-failure parity not established |
| `test_index_hash` | Streaming index fold plus corrupt/mismatched index fixtures; new CRC-correct reordered/shifted records retaining count and totals through sequential/parallel readers | Internal hash allocation/reset API is different; exhaustive upstream bad-index permutations not independently mapped assertion by assertion |
| `test_bcj_exact_size` | New exact-output/one-byte-input test across all eight filters and tails 0–17; pinned `good-1-empty-bcj-lzma2.xz` | Generated empty stream may have no block; the pinned fixture supplies the actual empty BCJ block |
| `test_memlimit` | `xz_container` memory/output-limit tests across sequential, seekable parallel and adaptive paths | Deliberate thread reduction and dictionary clamping differ from liblzma hard refusal; no lzip/auto decoder API |
| `test_lzip_decoder` | Not applicable | Lzip is not a supported container |
| `test_vli` | New nine independent upstream encoded vectors, all bit-width transitions, every truncation, noncanonical forms | Rust encoder documents panics for invalid output room/value rather than C error codes |
| `test_microlzma` | Not applicable | MicroLZMA is not a public codec here |
| `test_files.sh` | Existing complete `.xz`/`.lzma` manifest in `xz_utils`, six XZ reader modes, two LZMA reader modes, SHA-256 output oracle | Lzip files excluded because unsupported; CI must require fetched corpus |
| `test_suffix.sh` | Not applicable | CLI suffix handling is outside the library |
| `test_compress_generated_abc` | New exact `abc\n` generator and preset/filter matrix | CLI resource-limit option parsing is not ported |
| `test_compress_generated_random` | New exact wrapping-LCG generator and preset/filter matrix | Same |
| `test_compress_generated_text` | New exact word/paragraph generator and preset/filter matrix | Same |
| `test_scripts.sh` | Not applicable | xzgrep/xzdiff/xzless wrappers are not this library |

The three generated families reproduce `create_compress_files.c` and the
applicable `test_compress.sh` cases: presets 1–4, delta distances 1/4/256 and
all eight BCJ filters, decoded sequentially and with the seekable reader.
Existing `xz_encoder` independently checks generated output with xz/7zz and
covers checks, block sizes, threads and longer mixed corpora. Presets intentionally
use this crate's public preset mapping, not assumptions about SDK level numbers.

The four upstream OSS-Fuzz families map to `decode_lzma`, `decode_xz` and
`encode_round_trip`. The new `decode_xz` lane adds the previously missing
seekable parallel container path; adaptive parallel decoding was already present.
Unlike upstream's special fuzz build, production header/checksum verification is
not compiled out. Therefore equal target names do not imply equal deep-state
fuzz reach. Existing deterministic CRC-correct malformed fixtures complement it.
The audit also found that the scheduled runner used only three of the seven
registered fuzz binaries. `xtask/src/fuzz.rs` now schedules all seven, preserving
each target's actual input prefix (the raw LZMA2 MT prefix has three bytes,
not the sequential target's two). Its nonzero per-target mutation budgets sum
to the requested duration; startup/build time is additional. Each case has a
30-second timeout and each fuzzer a 1 GiB RSS limit. Findings do not prevent
later targets running. Runner tests check registration, finite budget totals
and real generated seed framing. Native runner tests and all seven fuzz-target
builds pass with the pinned SDK required. A new mutation campaign was not run
in this final validation; the scheduled fuzz jobs remain responsible for it.

## 7-Zip scope

The pinned upstream Git tree has no standalone unit/fixture test suite to port.
Files named `Test.bmp` are UI assets, not tests. SDK encoder/decoder oracles,
assembly differential tests, checked-versus-unchecked tests and generated 7zz
interoperability matrices therefore remain the concrete reference. This does
not claim coverage of unpublished upstream testing or all formats supported by
the 7zz application.

`sevenz-turbo/tests/differential_7zz_tests.rs` covers LZMA/LZMA2 levels, solid
and non-solid, filters, BCJ2, split volumes, AES/header encryption and PPMd.
Its security, damage, container, checksum, positional and folder-parallel suites
cover malformed metadata, extraction guards, cancellation and completion.
New `large_archive_tests` supplies actual large-file seek and delivery coverage.

## Intentional >4 GiB tests

| Test | What actually exceeds 4 GiB | Storage and work |
| --- | --- | --- |
| sevenz default sparse seek test | Real file length, packed-data position and next-header position | Only a few filesystem blocks allocated; six tiny extractions; Unix only |
| sevenz default truncated-header test | Real header file position | Sparse file, immediate parse rejection |
| sevenz ignored COPY test | Packed/member size and actual decoded byte count | Sparse archive; reads and verifies 4 GiB + marker; 64 KiB application buffer |
| sevenz ignored LZMA2 test | Member size and actual decoded byte count at T1 and T4 | 7zz receives 4 GiB + 65,537 generated bytes on stdin; small compressed archive; every decoded byte checked |
| LZMA ignored solid XZ test | One block's encoded input and decoded output size | 4 GiB + 65,537 input; sequential, parallel (T2) and adaptive (T2) decoders and independent xz SHA-256/count |
| LZMA ignored multiblock XZ test | Stream totals and cumulative output offsets | Same bytes over 257 blocks; sequential, parallel (T2) and adaptive (T2) decoders and independent xz SHA-256/count |

The XZ writer uses 64 KiB writes and a draining file sink with a hard 16 MiB
compressed-output cap; dictionary is 64 KiB. It never retains the full source
or decoded output in a test-owned vector. The 7zz compressed fixture has a
post-generation 16 MiB assertion, not a filesystem quota. Sparse files have
logical lengths above 4 GiB; they do not prove traversal of the hole unless the
ignored COPY test is run. Structural index tests are explicitly not large-data
decodes. Windows sparse generation remains a gap because NTFS requires an
explicit sparse-file operation to avoid physical zero filling.

These tests are deliberately ignored in ordinary CI. The two lzma-turbo ones
run weekly, and on demand, in `.github/workflows/large-streams.yml` against
the runner's system `xz`. Elsewhere, use an otherwise idle validation host
and run serially. Allow minutes per compressed case, not a
100 GiB fixture or a giant memory allocation. Actual timing/RSS measurements
must be recorded when run; these resource descriptions are design bounds,
not measured outcomes. No claim of bounded internal allocation follows solely
from fixed caller buffers: thread-start fallback and retained encoder state
still need allocation-lifetime validation.

```sh
cargo nextest run --locked -p lzma-turbo --test upstream_xz_boundaries --test xz_generated_corpus_round_trip --no-fail-fast
cargo nextest run --locked -p lzma-turbo --release --test large_streams --no-fail-fast --run-ignored ignored-only --test-threads=1
```

In sevenz-turbo:

```sh
cargo nextest run --locked --test large_archive_tests --no-fail-fast
cargo nextest run --locked --release --test large_archive_tests --no-fail-fast --run-ignored ignored-only --test-threads=1
```

Both slow compressed suites require the reference executable on PATH and fail
if it is unavailable; they never silently skip the independent reference.
No new dependency, production source, version, or fixture blob is introduced.

## Status and remaining evidence

The inventory and new cases pass the native macOS default workspace sweeps
on 2026-10-15: 456 LZMA and 507 Sevenz tests. Both explicit large XZ tests
pass (27.723 seconds total), as do all four sparse/large Sevenz archive tests
(13.282 seconds total), including actual data delivery above 4 GiB. These
are correctness-run durations, not performance acceptance measurements.
A later coverage measurement should map branch/line coverage
and unmatched applicable assertions (especially index hash and allocation
failures); counting files or test functions alone is insufficient evidence.

One confirmed compatibility defect was exposed by fixture construction:
sevenz-turbo refuses a streamed member when the entire optional SubStreamsInfo
section is omitted. The pinned SDK's `SzReadStreamsInfo` explicitly defaults
to one substream per folder in that case; `SzReadHeader2` derives its size from
the folder. A generated 26-byte COPY member with no section passed independent
`7zz t` and `7zz x -so` byte comparison. The expected-valid regression remains
in `sevenz-turbo/tests/upstream_optional_substreams.rs`. A separate faithful
parser correction now supplies the missing defaults, and the regressions are
enabled for validation, including multiple folders, optional inherited CRCs
and explicit malformed sections that must not fall back to defaults. Adding
the optional section to the large-offset fixtures alone did not close this
separate defect. The production fix and generated metadata regressions pass
the native workspace and focused post-lint validation.
