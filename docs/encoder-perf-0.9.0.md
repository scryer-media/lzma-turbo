# Encoder performance candidate

The implementation target is **0.9.0**. Provisional 0.9.1 build labels in
historical samples describe the binaries that were measured; they are not
release declarations. The samples, source hashes and rejected candidates
are retained. Machine identifiers have been replaced with role labels.

Review follow-up validation: 369 workspace tests passed (366 in the main
sweep plus three Wasm conformance tests), with two existing skipped tests;
four doctests passed. Both adaptive read-ahead regressions passed 100
repetitions each with retries disabled. Twenty release parity/golden tests
passed with `LZMA_TURBO_LZMA_UTIL_REQUIRE=1`, so absent external SDK oracles
cannot silently skip those checks. Formatting and workspace/all-feature
Clippy passed. This follow-up changes test synchronization, documentation
and version metadata; it does not alter the measured encoder implementation.
The historical round-8 count of 367 and round-9 count of 368 belong to
different test inventories, not conflicting totals for this follow-up.

The adaptive test now distinguishes cumulative claims from active workers.
A completion may free input memory and legitimately admit a fifth cumulative
claim. A separate deterministic unit test dispatches without collecting
completions: widening admits the fourth worker, and a fifth dispatch is
refused while all four slots remain outstanding. No retries or sleeps hide
failures, and decoder scheduling is unchanged.

## Round 10: zero-input T2/T4 input-allocation audit

The Zen 4 T4 outlier exposed an avoidable cost in the buffered probe's caller:
`std::fs::read` fills a complete 256 MiB allocation before entering the encoder.
That allocation receives ordinary pages. The SDK stream buffers and the Rust
encoder's own large buffers request huge pages before touching them. The
borrowed-slice encoder cannot recover time already spent preparing its input.

With the same `e2ab3ab` encoder, a separate caller-buffer variant requests
`MADV_HUGEPAGE` for the aligned interior of its owned allocation before filling
it. It still reads every input byte, retains the entire input and output, and
includes allocation, initialization, reading, encoding and destruction in
whole-process measurements. The original probe and all previous results remain
unchanged. This is a Linux fixed-file diagnostic, not a library API change or a
sevenz-turbo integration change.

Nine interleaved uninstrumented runs per arm, fresh Zen 4 and Sapphire Rapids
hosts, level 5, 256 MiB zeros. Wall and CPU compare official 7zz; RSS compares
the matching SDK memory-input/memory-output probe with `Z7_LARGE_PAGES` enabled.

| CPU / threads | Original buffered wall, s | Huge-page caller wall / 7zz, s | Wall delta | CPU delta | Buffered RSS delta |
| --- | ---: | ---: | ---: | ---: | ---: |
| Zen 4 T2 | 0.748543 | 0.647505 / 0.853685 | -24.15% | -20.63% | +0.07% |
| Zen 4 T4 | 0.463146 | 0.378183 / 0.468652 | -19.30% | -21.36% | +0.31% |
| Sapphire Rapids T2 | 1.014896 | 0.964172 / 1.190745 | -19.03% | -13.95% | +0.09% |
| Sapphire Rapids T4 | 0.739464 | 0.713363 / 0.814438 | -12.41% | -16.46% | +0.32% |

Each of these four caller-buffer variant rows meets all three 3% limits. It
does not requalify the rest of the matrix with this new input policy. Intel T2
CPU increased 2.36% against its same-batch original Rust caller, while remaining
13.95% below official 7zz; the input-policy change does not improve every metric
against the original caller on every CPU.

Separate phase probes isolate the mechanism:

| CPU / threads | Original / advised input read, ms | Original / advised input free, ms | Original / advised encoding, ms |
| --- | ---: | ---: | ---: |
| Zen 4 T2 | 113.460 / 30.846 | 18.215 / 0.985 | 609.562 / 609.355 |
| Zen 4 T4 | 113.668 / 30.966 | 17.952 / 0.974 | 331.920 / 347.630 |
| Sapphire Rapids T2 | 126.351 / 87.523 | 13.168 / 0.939 | 779.073 / 854.867 |
| Sapphire Rapids T4 | 126.896 / 87.417 | 12.912 / 0.873 | 615.868 / 631.546 |

On Zen 4, input minor faults fall from 65,541 to 644. The approximately 132 ms
serial input lifetime cost is almost identical at T2 and T4, while encoding
roughly halves at T4. That fixed cost consumes much more of the T4 advantage.
The advised allocation recovers about 100 ms of input lifetime cost; scheduling
variation means that phase medians need not sum to process medians. Encoding
itself is not faster in every advised-buffer row. Setup is only about 0.16 ms;
encoder destruction is 1.4–2.9 ms. Zen 4 provides four physical cores here;
the Intel host provides two physical cores with two hardware threads each.

The source audit covered the caller's allocation/read/drop, property and block
normalization, memory preparation, direct-input finder ownership, hash/tree/LZ
handoffs, block startup and ordered completion, direct final output regions,
compaction, shrinking and encoder teardown, against `Lzma2Enc_Encode2`,
`Lzma2Enc_EncodeMt1`, `Lzma2Enc_MtCallback_Code`, `ThreadFunc2`, `BigAlloc` and
the match-finder synchronization. T4 already borrows its input and writes to
final output regions; it has no extra whole-input copy. The observed large
recoverable cost belongs to the input allocation before those paths execute.

The unchanged buffered Zen 4 T4 baseline exceeded the wall gate in the first
two diagnostic batches: +6.52% and +3.49%. Its later better results do not erase
those failures or establish a robust pass for the original caller. Initial
phase batches also had lightweight waiting/collection controllers; the final
uninstrumented stage started after both previous stages completed. The initial
C control omitted `Z7_LARGE_PAGES`; both it and the corrected control are saved,
and C-probe CPU never substitutes for official 7zz CPU.

T2 and T4 complete outputs are byte-identical across the original caller,
advised caller, streaming encoder and SDK buffered API on both hosts. Full
decompression matches the entire 256 MiB input. Executable hashes match between
hosts. All 756 samples, phase counters, compiler/kernel/page-policy metadata,
build logs, binary and output hashes, and exact diagnostic sources/scripts are
in `encoder-zero-audit-0.9.0.json`. The reusable fixed-file caller variant is
`bench/encoder-buffered-huge-probe.rs`. No encoder source changed in this audit.

Both round-10 hosts are confirmed terminated. This audit used 0.496
launch-to-termination-request instance-hours; cumulative use is 28.634
instance-hours across 20 terminated hosts. Formatting and workspace Clippy
pass. The unchanged encoder retains the round-9 full-suite validation below;
this audit adds the native differential and full-decompression checks above.

## Historical round 9 acceptance matrix

Round 9 passes the requested 3% wall-time, whole-process CPU and peak-RSS gates independently in every measured case: seeded and zero-filled 256 MiB inputs, buffered and streaming APIs, T2 and T4, plus 16 MiB T1 coverage, on Zen 4 and Sapphire Rapids. Each timing arm has seven interleaved runs. Buffered RSS compares the matching SDK memory-input/memory-output API; streaming RSS compares official 7zz.

Base: `ef7fed9`, target version `0.9.0`. Reference: SDK 26.03 at `0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17`. Level 5, 32 MiB dictionary. CPU is complete-process user plus system time; wall time includes input acquisition, encoding and teardown. Every sample records process peak RSS. No sampled profile contributes to acceptance.

Coverage is limited to these LZMA2 encoder cases on Linux x86-64. It does not establish sevenz-turbo end-to-end archive performance, decoder performance, other levels or corpora, or ARM/macOS/Windows performance. The measured build originally carried a provisional 0.9.1 label; the maintainer has retargeted these changes to 0.9.0. Historical build labels and source hashes remain evidence of what was measured, not a release declaration. The original measurements and executable hashes remain unchanged in the JSON.

## Buffered results

All figures are medians. CPU and wall use official 7zz as the denominator. The SDK buffered RSS reference has three runs per case.

| CPU | Input / threads | Rust / 7zz wall, s | Wall delta | Rust / 7zz CPU, s | CPU delta | Rust / SDK buffered RSS, MiB | RSS delta |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Zen 4 | Seeded T2 | 25.0118 / 25.2907 | -1.10% | 41.56 / 41.03 | +1.29% | 807.97 / 806.73 | +0.15% |
| Zen 4 | Seeded T4 | 13.0178 / 13.2951 | -2.09% | 42.49 / 42.40 | +0.21% | 1149.89 / 1355.73 | -15.18% |
| Zen 4 | Zeros T2 | 0.7493 / 0.8558 | -12.45% | 1.59 / 1.88 | -15.43% | 583.72 / 583.17 | +0.10% |
| Zen 4 | Zeros T4 | 0.4687 / 0.4682 | +0.12% | 1.39 / 1.67 | -16.77% | 911.59 / 908.61 | +0.33% |
| Sapphire Rapids | Seeded T2 | 32.0219 / 32.9671 | -2.87% | 56.32 / 55.40 | +1.66% | 808.19 / 806.73 | +0.18% |
| Sapphire Rapids | Seeded T4 | 20.7564 / 20.5869 | +0.82% | 73.07 / 71.74 | +1.85% | 1149.83 / 1355.85 | -15.20% |
| Sapphire Rapids | Zeros T2 | 0.8677 / 1.1595 | -25.16% | 1.92 / 2.29 | -16.16% | 583.81 / 583.11 | +0.12% |
| Sapphire Rapids | Zeros T4 | 0.7454 / 0.8441 | -11.69% | 2.29 / 2.80 | -18.21% | 911.55 / 908.61 | +0.32% |

The official command streams its input and output, so its RSS is not a like-for-like buffered denominator. Its wall and CPU remain the acceptance reference. Seeded T4 and zero T2 SDK buffered RSS were measured earlier on the same hosts using the unchanged matching probe; seeded T2 and zero T4 have final paired RSS checks. No diagnostic C-probe CPU result replaces official 7zz CPU. All raw samples, including official-command RSS, are retained in the JSON.

## Streaming results

Each row also has seven interleaved runs per arm. All three metrics compare matching file-stream paths.

| CPU | Input / threads | Rust / 7zz wall, s | Wall delta | Rust / 7zz CPU, s | CPU delta | Rust / 7zz RSS, MiB | RSS delta |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Zen 4 | Seeded T2 | 23.8382 / 25.3083 | -5.81% | 39.91 / 41.04 | -2.75% | 377.94 / 380.95 | -0.79% |
| Zen 4 | Seeded T4 | 12.4523 / 13.2466 | -6.00% | 41.02 / 42.37 | -3.19% | 1132.36 / 1135.91 | -0.31% |
| Zen 4 | Zeros T2 | 0.6610 / 0.8576 | -22.92% | 1.49 / 1.89 | -21.16% | 377.95 / 381.00 | -0.80% |
| Zen 4 | Zeros T4 | 0.3759 / 0.4680 | -19.68% | 1.33 / 1.68 | -20.83% | 909.52 / 912.38 | -0.31% |
| Sapphire Rapids | Seeded T2 | 31.1056 / 32.8723 | -5.37% | 53.78 / 54.93 | -2.09% | 377.97 / 381.03 | -0.80% |
| Sapphire Rapids | Seeded T4 | 19.8918 / 20.6430 | -3.64% | 70.66 / 71.48 | -1.15% | 1132.43 / 1135.82 | -0.30% |
| Sapphire Rapids | Zeros T2 | 0.9491 / 1.1496 | -17.44% | 1.92 / 2.30 | -16.52% | 377.96 / 380.96 | -0.79% |
| Sapphire Rapids | Zeros T4 | 0.6885 / 0.7813 | -11.88% | 2.26 / 2.63 | -14.07% | 909.57 / 912.34 | -0.30% |

## Single-thread 16 MiB coverage

Level-5 LZMA2, seeded and zero-filled inputs, seven complete-process runs per timing arm and three matching SDK buffered RSS runs. CPU uses direct wait4 process accounting at microsecond precision for these shorter rows. Official 7zz uses explicit `-mmt1 -ms=on` to match the API single-thread solid-block plan.

| CPU | API / input | Rust / 7zz wall, s | Wall delta | Rust / 7zz CPU, s | CPU delta | Rust / matching SDK RSS, MiB | RSS delta |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Zen 4 | Buffered seeded | 1.8029 / 3.8420 | -53.07% | 1.8026 / 3.8415 | -53.08% | 195.66 / 192.11 | +1.85% |
| Zen 4 | Buffered zeros | 0.0874 / 0.3849 | -77.30% | 0.0872 / 0.3847 | -77.32% | 181.52 / 178.17 | +1.88% |
| Zen 4 | Streaming seeded | 1.7552 / 3.8300 | -54.17% | 1.7549 / 3.8296 | -54.18% | 181.79 / 181.80 | -0.01% |
| Zen 4 | Streaming zeros | 0.0816 / 0.3851 | -78.80% | 0.0812 / 0.3849 | -78.90% | 181.75 / 181.65 | +0.06% |
| Sapphire Rapids | Buffered seeded | 2.2313 / 5.4333 | -58.93% | 2.2308 / 5.4328 | -58.94% | 195.60 / 192.11 | +1.82% |
| Sapphire Rapids | Buffered zeros | 0.1108 / 0.4227 | -73.78% | 0.1105 / 0.4223 | -73.85% | 181.53 / 178.11 | +1.92% |
| Sapphire Rapids | Streaming seeded | 2.1687 / 5.5572 | -60.97% | 2.1681 / 5.5564 | -60.98% | 181.86 / 181.76 | +0.05% |
| Sapphire Rapids | Streaming zeros | 0.1083 / 0.4246 | -74.48% | 0.1080 / 0.4244 | -74.55% | 182.48 / 181.73 | +0.41% |

## Complete path audit and retained changes

The audit covers input/output ownership, properties and block planning, allocation and initialization, hash and tree producers, match mixing and skipping, both parsers, probability/price updates, range coding, chunk output, scheduling, compaction, reuse, cancellation and failure lifetimes. The combined retained changes are:

- Write T4 blocks directly into their bounded final output regions, with the end marker only on the last block. This removes chunk staging and a full payload copy.
- Keep the skip loop within its held block, specialize by hash width, hold cursors in locals and write them back before handoff or return. Check the masked hash extent once per block and the exact input-byte extent once per position.
- Extend matches with bounded word comparisons at all eight parser sites, preserving the checked scalar oracle and exact tails.
- Keep the tree cut counter and invariant input base in general registers; retain SDK child-load order, error precedence and long-match copying. Use direct dispatch for the common BT4 mixer.
- Update actual probability cells at all 25 range-coder sites and select the bound/remainder directly, retaining identical low/probability arithmetic and normalization.
- For immutable direct input, omit only movable-window critical sections and pending-shift atomic swaps. Semaphore ownership, start/stop events, block counts and failure behavior remain intact. Streaming retains the window locks and shifts.

The final buffered round added no further unsafe blocks, dependencies, eager scans, copies, page promotion or allocations. The complete change does add unsafe assembly, parser/window accesses, low-hash accesses and finder variant assumptions; their tests and native-only versus Miri coverage are enumerated in AGENTS.md. Native tests cover copied/direct equivalence, every hash width, exact tails, malformed tables, short output, reuse, producer panic and cancellation.

## Rejected candidates and measurement integrity

All intermediate source hashes and measurements remain in the JSON. The full eight-way tree/mixer/literal-loop comparison rejected fixed-count literal loops. Candidate D failed seven-run Intel T4 CPU at +3.77%; E at +4.52%; G at +3.45%; H at +3.96%. Both SIMD-child-retention F variants failed. These results are not replaced by better later runs.

I-H passed the seeded CPU checks but failed Intel buffered zero T2 wall time: 1.1488 / 1.0627 seconds (+8.10%), despite lower CPU. That result is explicitly rejected. Remaining diagnostic RSS/parity work was cancelled after preserving the timing matrix; partial rows remain diagnostic. J adds the complete direct-input handoff and skip-loop changes. Its zero comparisons include I-H, J and official 7zz together, seven runs per arm; seeded guards compare J and 7zz. A later I-H comparison does not supersede its earlier failure.

Separate zero-input cpu-clock profiles placed 34–39% of CPU in the skip loop, 30–38% in the tree and 14–16% in hashing, with no lost samples. Earlier seeded profiles placed 54–59% in the tree. Sampling is excluded from all acceptance timing. Earlier B two-arm screens had an ordering defect; the corrected confirmations rotate arms and reverse complete cycles.

The initial T1 reference used XZ auto blocks with `-mmt1`, which still ran a two-thread LZMA encoder. In `XzProps_Normalize`, the auto-block branch normalizes a temporary copy without carrying its one-thread setting into the live LZMA properties. Explicit solid mode forwards the thread limit correctly. Paired process accounting confirms one CPU in solid mode, and its official XZ payload equals the SDK raw payload. All original mismatched-thread rows and both timing harnesses remain in the JSON; acceptance uses separately named matching-thread rows with unchanged J binaries.

## Validation and provenance

Round-9 validation: 368 Nextest tests and four doctests passed, with two existing ignored tests. Formatting, default and all-feature Clippy, required release SDK parity, golden vectors, no-default, encoder-only and both Wasm checks passed. Both EC2 CPUs passed all 173 native release unit tests before timing. Miri remains CI-only.

Complete seeded and zero-filled T2/T4 payloads match the prior encoder, the SDK buffered API and the current streaming path byte for byte. Every arm decompresses to the entire original 256 MiB input. The additional 16 MiB T1 buffered/streaming/SDK payloads also match and fully decompress. All twelve encoder-source and probe hashes were checked on both hosts. The remote CI workflow is an older non-build copy; that mismatch and the initial failed attestation are explicitly retained. It is not an encoder-source mismatch.

Buffered executable `.text` SHA-256: `d393f6976d196f6c39b34f1595f485a9d977138ca7fd1c719d6cd9a0ad258d45`.
Streaming executable `.text` SHA-256: `8c89f444cff87a8a300b26a3c864893acd94595f767deae753cbea23d02cabba`.

Both round-9 hosts were terminated by explicit instance ID and confirmed terminated. Round-9 use: 11.872 instance-hours. Cumulative use: 28.138 instance-hours across 18 hosts. These are actual launch-to-termination-request lifetimes, including idle time, not a billing estimate.

## Historical round 8

The following results belong to the frozen round-8 source, not the current candidate. Earlier failures remain visible.
### Historical buffered T2 results

256 MiB seeded corpus, level 5. Seven interleaved runs per Rust and official 7zz arm; three runs of the matching SDK memory API. CPU is whole-process user plus system time. The CPU denominator is official 7zz; RSS is also compared with SDK retaining the same complete input and output.

| CPU | Before / final / SDK buffered RSS, MiB | RSS saved, MiB | CPU vs official 7zz | Wall vs official 7zz |
| --- | ---: | ---: | ---: | ---: |
| Zen 4 | 857.58 / 807.95 / 806.86 | 49.63 | +2.82% | +1.54% |
| Sapphire Rapids | 857.71 / 808.20 / 806.67 | 49.52 | +1.97% | -0.86% |

The accepted RSS ceiling is also +3% against the matching SDK memory API. Measured final RSS deltas are Zen 4 +0.14%, Sapphire Rapids +0.19%.

| CPU | Before / final / official 7zz wall, s | Before / final / official 7zz CPU, s |
| --- | ---: | ---: |
| Zen 4 | 25.1202 / 26.0935 / 25.6981 | 41.49 / 42.68 / 41.51 |
| Sapphire Rapids | 31.1470 / 32.4954 / 32.7782 | 54.57 / 55.97 / 54.89 |

The RSS reduction trades some CPU and wall time against the previous buffered implementation; the measured final CPU remains within the operator’s SDK ceiling. These figures do not claim equality with SDK’s roughly 381 MiB streaming command, which does not retain the complete input and output.

### Historical shared-path checks

T4 uses seven runs per arm. Streaming uses three runs per arm on this exact source.

| CPU | API / threads | Rust / SDK wall, s | CPU delta | Rust / SDK CPU, s | Rust / SDK RSS, KiB |
| --- | --- | ---: | ---: | ---: | ---: |
| Zen 4 | Buffered T4 | 13.6688 / 13.4254 | +2.81% | 43.90 / 42.70 | 1177524 / 1163152 |
| Zen 4 | Streaming T2 | 24.9954 / 25.5660 | -0.58% | 41.22 / 41.46 | 387024 / 390096 |
| Zen 4 | Streaming T4 | 13.0416 / 13.4736 | -0.77% | 42.44 / 42.77 | 1159996 / 1163204 |
| Sapphire Rapids | Buffered T4 | 20.9665 / 20.2777 | +4.39% | 73.94 / 70.83 | 1177540 / 1163020 |
| Sapphire Rapids | Streaming T2 | 29.6458 / 31.4094 | -0.92% | 52.79 / 53.28 | 387140 / 390120 |
| Sapphire Rapids | Streaming T4 | 19.8240 / 20.3491 | +3.62% | 71.86 / 69.35 | 1160036 / 1163104 |

The Intel streaming T4 three-run result above triggered a seven-run, interleaved previous/current/SDK check on the same host, with both executable hashes verified before timing. Its whole-process CPU medians were 71.05 / 71.26 / 69.52 s; wall 19.8303 / 19.9307 / 20.0524 s; peak RSS 1159296 / 1159940 / 1163044 KiB. Final CPU is +2.50% versus SDK and +0.30% versus previous, with wall +0.51% versus previous and 0.61% faster than SDK. This confirmation meets the revised 3% CPU ceiling, but not the earlier 2% ceiling. The initial +3.62% result remains above and in the raw data.

The Intel buffered T4 absolute CPU gate did not pass on this recovery host: final +4.39% versus SDK, with the unchanged previous implementation also at +4.24%. Final versus previous CPU changed +0.15% and wall time improved 0.29%. This is a remaining absolute SDK gap, not a passing T4 result. The requested buffered T2 gate passes on both hosts.

Seven-run zero-filled T2 diagnostics, before / final / SDK:

| CPU | Wall, s | CPU, s | RSS, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 1.2566 / 1.1455 / 0.8571 | 2.57 / 2.41 / 1.88 | 649180 / 597720 / 390080 |
| Sapphire Rapids | 1.7327 / 1.6072 / 1.1781 | 2.71 / 2.61 / 2.32 | 649124 / 597784 / 390096 |

The zero-filled diagnostic remains a separate limit on generalizing the seeded-corpus CPU gate; it is not omitted from the raw evidence. Both seeded and zero-filled T2 output match the SDK byte for byte, and complete T2/T4 payload hashes and decompression match the reference.

Historical round-8 validation (before the additional round-9 test): 367 Nextest tests and four doctests passed, with two existing ignored tests. Formatting, default and all-feature Clippy, required release SDK parity, golden vectors, no-std, encoder-only and Wasm checks passed. Miri remains a CI-only check.

The buffered executable `.text` SHA-256 is `0f346d250fe8a27352f92b2fc3130b84f6776eb04c848aa019c536d93531d3c0`. Source hashes and every sample are in the companion JSON. The first AMD screen used a stale library artifact and is explicitly excluded; rebuilt executables had to match across hosts. Failed performance experiments remain identified separately.

## Buffered T2: memory input and final output

The full solid-block path now follows the SDK's `inData`/`outBuf` route:
`encode_to_vec` → `encode_solid_to_vec` → `encode_mt1_mem_into` →
`mem_prepare_direct` → `run_block` → `encode_subblock_into` →
`code_one_mem_block`. The prior route wrapped the input in `SliceStream`
and entered `encode_mt1_stream`, which allocated and filled a second input
window. The SDK comparison is `Lzma2Enc_Encode2` → `Lzma2Enc_EncodeMt1` →
`LzmaEnc_MemPrepare` → `Lzma2EncInt_EncodeSubblock` with memory destinations.

The combined changes remove that input copy, reserve one bounded final
output, write compressed payloads directly after their headers, and allocate
the scratch chunk buffer only for stream output. Stored chunks overwrite
the attempted compressed payload in the destination, then restore coder
state exactly as the SDK does. The range encoder's own 64 KiB buffer remains. Both output paths now feed the same checked-slice writer, which advances a remaining slice like the SDK pointer and remaining length. Stream scratch is initialized once, avoiding Vec growth checks on each flush.
No library unsafe code or dependencies are added in this follow-up.

Allocation inventory for a fresh encoder, 256 MiB input, level 5, 32 MiB
dictionary, two threads:

| Allocation | Previous Rust | Candidate Rust | SDK memory API |
| --- | ---: | ---: | ---: |
| Caller input | 256 MiB | 256 MiB | 256 MiB |
| Compressed result length | 223.605 MiB | 223.605 MiB | 223.605 MiB |
| Copied input window | 50.125 MiB | 0 | 0 |
| Tree and hash allocation | 335,810,624 B | unchanged | 335,810,624 B |
| Finder handoff buffers | 5,242,888 B | unchanged | 5,242,880 B |
| Range buffer | 65,536 B | unchanged | 65,536 B |
| Staged chunk buffer | 65,552 B | 0 | 0 |
| Literal probabilities and saved copy | 24,576 B | unchanged | 24,576 B |

The remaining parser, price, position and synchronization state is small
relative to these allocations and remains unchanged. Output capacity is
268,697,616 bytes before trimming; capacity does not mean all those pages
are resident. GNU time measures actual peak RSS separately below. Avoiding
staging also removes approximately 224 MiB of output-copy traffic on this
input, even though the staging allocation itself is only about 64 KiB.

Both implementations retain finder allocations and threads for reuse. A
Rust encoder previously used for streaming can retain its old input window,
as the SDK does; the fresh-instance measurements do not claim to reclaim
that cache. Fast/single-threaded finders that cannot borrow the input retain
their copying input path. Explicit non-solid block configurations keep their
existing scheduler. Failed worst-case output reservations fall back to a
growing destination with direct input.

The buffered API necessarily retains its input and compressed result,
about 480 MiB together here. Its RSS therefore needs a memory-input/output
SDK comparator, in addition to the official streaming command. The CPU
acceptance denominator remains official 7zz, not the slower diagnostic C
build. `bench/sdk-buffered-probe.c` uses the pinned SDK and keeps the same
complete input and output alive during encoding. Compile it with:

```bash
sdk=target/lzma-util/sdk/C
cc -O2 -D_GNU_SOURCE -pthread -I "$sdk" bench/sdk-buffered-probe.c \
  "$sdk"/{Alloc,CpuArch,LzFind,LzFindMt,LzFindOpt,LzmaEnc,Lzma2Enc,MtCoder,MtDec,7zStream,Threads}.c \
  -o target/sdk-buffered
TRACE_ALLOC=1 target/sdk-buffered 2 2 bench/fixtures/p256.bin
```

Allocation tracing runs separately from timing. The C probe is an RSS and
byte-parity comparator, compiled without official 7-Zip assembly; its CPU
numbers do not replace the official binary's acceptance gate.

The sections below are earlier measurements of their separately frozen source snapshots. Their use of “current” refers to that round, not the final T2 implementation above.

## Earlier accepted streaming wall, CPU and RSS results

| CPU | Threads | Rust / SDK wall, s | Wall delta | Rust / SDK CPU, s | CPU delta | Rust / SDK peak RSS, KiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Zen 4 | 2 | 24.8866 / 25.5090 | -2.44% | 41.17 / 41.34 | -0.41% | 387036 / 390176 |
| Zen 4 | 4 | 13.0231 / 13.3624 | -2.54% | 42.32 / 42.56 | -0.56% | 1159588 / 1163244 |
| Sapphire Rapids | 2 | 22.1706 / 23.5587 | -5.89% | 40.94 / 41.76 | -1.96% | 387036 / 390220 |
| Sapphire Rapids | 4 | 15.7655 / 16.0811 | -1.96% | 56.79 / 56.31 | +0.85% | 1159252 / 1163196 |

The largest CPU gap is +0.85%, on Sapphire Rapids T4. Both executables read
a file and discard output through a file stream. Rust emits raw LZMA2;
7-Zip also performs XZ container and CRC work. These figures do not claim
that the buffered whole-input/whole-output API has the same cost. Its
separate measurements are reported below. Instruction counts are diagnostics,
not the wall/CPU acceptance criterion.

## Before the bounded word comparison

Seven interleaved unsampled runs, 256 MiB, SDK level 5, matching file input
and discarded file output. CPU seconds are user plus system time across all
threads; RSS is the median process maximum, in KiB.

| CPU | Threads | Rust wall / SDK wall, s | Rust CPU / SDK CPU, s | Rust RSS / SDK RSS, KiB |
| --- | ---: | ---: | ---: | ---: |
| Zen 4 | 2 | 25.7254 / 25.5637 | 41.95 / 41.27 | 387028 / 390108 |
| Zen 4 | 4 | 13.4934 / 13.3150 | 43.24 / 42.44 | 1159860 / 1163188 |
| Sapphire Rapids | 2 | 23.6432 / 23.7126 | 42.51 / 41.80 | 387148 / 390164 |
| Sapphire Rapids | 4 | 16.1893 / 16.0433 | 58.05 / 56.38 | 1159300 / 1163068 |

All four wall medians pass. CPU deltas are +1.65%, +1.89%, +1.70% and
**+2.96%**, respectively; the last row fails. RSS is slightly below SDK.
These results describe the cached window, trusted mixer, match-pair margin
and local literal-range candidate. The subsequent bounded word comparison
is retained: its initial three-run T4 wall/CPU medians are 13.0964/42.40 s
on Zen 4 versus SDK's 13.3364/42.51 s, and 15.8817/56.83 s on Sapphire Rapids
versus SDK's 15.8413/56.94 s. Peak RSS is 1159128/1163116 KiB and
1159412/1163100 KiB respectively (Rust/SDK). Its subsequent seven-run
confirmation missed Sapphire Rapids T4 CPU by +2.32%. The final 16/8-byte
scan and output-handle binding supersede it, as shown in the final table.

## Separate single-thread and buffered API results

Single-thread LZMA, 16 MiB, SDK level 5, pinned to logical CPU 0: seven
interleaved runs, whole-process time. The reference is the pinned SDK C
oracle built with `cc -O2 -DZ7_ST -D_7ZIP_ST`; this is a different reference
build from the official 7-Zip binary above. Rust retains complete input and
output; the C oracle writes LZMA-Alone to `/dev/null`. Raw output matches
byte for byte after removing the reference's 13-byte header, and decodes to
the input on both processors.

| CPU | Rust / SDK C wall, s | Rust / SDK C CPU, s | Rust / SDK C peak RSS, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 1.8286 / 4.3178 | 1.82 / 4.30 | 218892 / 231432 |
| Sapphire Rapids | 1.6860 / 3.9030 | 1.67 / 3.89 | 218880 / 231436 |

The buffered LZMA2 API retains the complete 256 MiB input and compressed
output. Before the ownership change below, its separate T4 results used
three interleaved runs:

| CPU | Rust / SDK wall, s | Rust / SDK CPU, s | Rust / SDK peak RSS, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 13.6245 / 13.3681 | 43.76 / 42.58 | 1389612 / 1163240 |
| Sapphire Rapids | 16.2868 / 15.8551 | 58.66 / 56.95 | 1389452 / 1163096 |

That candidate's buffered CPU was +2.77% on Zen 4 and +3.00% on Sapphire
Rapids; wall was +1.92% and +2.72%. It missed the original 2% target against
the file-stream reference, with peak RSS about 221 MiB higher.

## Earlier buffered block destinations (round 6)

Block workers now write into disjoint regions of one output allocation,
using the SDK's `blockSize + (blockSize >> 10) + 16` destination bound.
Each region is an ordinary checked mutable slice. After workers join, the
written prefixes are compacted in order with `copy_within`, and the result
is truncated and shrunk. The existing lazy zeroed allocator avoids eagerly
touching unwritten regions. No unsafe code or dependencies are added.

Coder reuse, block scheduling and the single-block path are unchanged.
If the worst-case contiguous reservation cannot fit, the previous
ownership-transfer implementation remains the fallback. Generic stream
sinks keep their existing buffer-reuse contract.

Seven interleaved runs per arm, 256 MiB, level 5, T4. Each cell lists the
previous accepted buffered implementation / current implementation / SDK:

| CPU | Wall, s | Total CPU, s | Peak RSS, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 13.5487 / 13.4752 / 13.2469 | 43.63 / 43.54 / 42.44 | 1274988 / 1177520 / 1163192 |
| Sapphire Rapids | 16.9424 / 16.7727 / 16.4998 | 59.81 / 59.63 / 58.12 | 1274904 / 1177584 / 1163048 |

T4 peak RSS falls another 95.18 MiB and 95.04 MiB, respectively. The
remaining SDK gap is 13.99 MiB and 14.20 MiB, about 1.2–1.3% of SDK RSS.
CPU is +2.59% and +2.60% above SDK, with wall time +1.72% and +1.65%.
Compared with the previous build, CPU falls 0.21% and 0.30%, and wall time
falls 0.54% and 1.00%.

The same seven-run comparison confirms T2, whose single-block path is
unchanged. Each cell again lists previous / current / SDK:

| CPU | Wall, s | Total CPU, s | Peak RSS, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 24.6733 / 24.6873 / 25.1642 | 41.03 / 41.00 / 40.92 | 878312 / 878156 / 390096 |
| Sapphire Rapids | 23.3929 / 23.5015 / 24.0538 | 42.37 / 42.61 / 42.65 | 878208 / 878200 / 390100 |

T2 CPU is +0.20% and -0.09% relative to SDK. Relative to the previous
buffered build, CPU changes by -0.07% and +0.57%; wall changes by +0.06%
and +0.46%. T2 memory remains unchanged: this API retains the complete input
and output, and these results do not claim T2 RSS parity with SDK.

The initial three-run screening sample showed Sapphire Rapids CPU +3.34%;
it is retained in the JSON and was not accepted as meeting the ceiling.
The preplanned seven-run confirmation above uses the same unchanged source.
The complete buffered release `.text` hash on both processors is
`1a61798ee26411fc61de9f726e17228cb90d04708fd64a822f967af957d065e1`.

A separate zero-filled 256 MiB T4 diagnostic checks whether the large
reservation becomes resident for tiny output. Seven interleaved runs,
previous / current / SDK:

| CPU | Wall, s | Total CPU, s | Peak RSS, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 0.6783 / 0.7236 / 0.4667 | 2.17 / 2.33 / 1.68 | 931492 / 933548 / 934212 |
| Sapphire Rapids | 0.9105 / 0.8935 / 0.7014 | 2.80 / 2.71 / 2.32 | 931384 / 933440 / 934204 |

The final RSS is about 2 MiB higher than before and remains below SDK.
The 39,231-byte output matches the previous build. This diagnostic does
**not** meet the 3% CPU ceiling: both old and new buffered builds exceed
SDK. The new build adds 0.16 CPU seconds (+7.37%) on Zen 4 and saves 0.09
seconds (-3.21%) on Sapphire Rapids. The seeded-corpus acceptance above
must not be read as a CPU guarantee for every input. The initial three-run
zero-filled diagnostic, which also wrote output to a file, is retained
separately in the JSON.

## Earlier buffered output ownership

The follow-up target allows buffered CPU up to 3% above SDK while reducing
RSS without a material CPU regression. The first change took ownership
of the first completed block's allocation when multiple block threads run.
It appends subsequent blocks in order. This removes a duplicate allocation
and its copy; generic output streams retain their existing buffer-reuse
contract. The single-block path is unchanged. The change adds no unsafe
code or dependencies.

Seven interleaved runs per arm, 256 MiB, level 5, T4. Each cell lists
before / after / official SDK; RSS is the median process maximum:

| CPU | Wall, s | Total CPU, s | Peak RSS, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 13.8477 / 13.7771 / 13.5688 | 44.09 / 44.15 / 43.10 | 1389424 / 1274980 / 1163172 |
| Sapphire Rapids | 15.7215 / 15.7583 / 15.6912 | 57.30 / 56.76 / 55.35 | 1389444 / 1274908 / 1163092 |

T4 peak RSS falls by 111.76 MiB on Zen 4 and 111.85 MiB on Sapphire Rapids.
CPU is +2.44% and +2.55% above SDK, meeting the revised ceiling. Relative to
the previous buffered implementation, CPU changes by +0.14% and -0.94%;
wall changes by -0.51% and +0.23%. The largest peak RSS across the seven
candidate runs is 1275056 KiB and 1275008 KiB, respectively.

The same seven-run comparison confirms the single-block T2 path:

| CPU | Wall before / after / SDK, s | Total CPU before / after / SDK, s | Peak RSS before / after / SDK, KiB |
| --- | ---: | ---: | ---: |
| Zen 4 | 25.1137 / 25.0414 / 25.7664 | 41.60 / 41.47 / 41.59 | 878232 / 878112 / 390060 |
| Sapphire Rapids | 21.6415 / 21.5623 / 22.4486 | 40.04 / 39.93 / 40.52 | 878240 / 878240 / 390096 |

T2 CPU is 0.29% and 1.46% below SDK, respectively, and 0.31% and 0.27%
below the previous buffered build. RSS is unchanged within run variation.
All four buffered rows meet the 3% CPU ceiling; the largest wall delta
against SDK is +1.54%, on Zen 4 T4.

The remaining T4 RSS excess over SDK is about 109 MiB. This API still owns
its complete input and output, while the SDK command streams to `/dev/null`.
The saving describes this workload; later blocks can reuse or replenish
the worker buffers, so other block counts need their own measurements.

Two additional experiments reduced T2 RSS by about 50 MiB through direct
input or the existing single-block worker path, but increased CPU time.
Both were rejected. Their screening rows remain in the JSON report.

The final streaming probe's entire release `.text` is byte-identical to
the previously accepted streaming binary on both processors, SHA-256
`4e8fd5a0f99ff7027b45ed1f2146729c7e768508fb1089a1f9c7714f52cfb240`.
The buffered binary has its own fingerprint and measurements.

## Tree walk

The x86-64 kernel now directly ports `GetMatchesSpecN_2` from the pinned
SDK's `Asm/x86/LzFindOpt.asm`, including its negative lengths, dependent-load
order, output cursor, and long-match copy path. The assembly is in
`src/enc/bt_kernel_x86_64.S`; the Rust wrapper establishes the window, tree,
head, position, and output-room invariants once. Inputs outside its margin
use the checked slice implementation. Zero heads and non-descending children
preserve the checked implementation's refusal order. Debug builds also
preserve its zero-cut-counter overflow behavior.

After two equal bytes, the length extension compares 16 bytes with baseline
x86-64 SSE2 when its negative offset is at most -16, then eight bytes when
at most -8. Both reads stay below the checked match limit. The SSE2 equality
mask or word XOR identifies the first differing byte with `bsf`; a byte
comparison preserves the SDK's ordering branch. The final one to seven bytes
use the scalar loop. The portable Rust `scan` helper retains its bounded
eight-byte comparison. Tree traversal, emitted matches and long-match copying
are unchanged.

The emitted Linux object has no stack operand in the dependent node path
(offsets 0x120–0x168). `son` stays in r9. The following next-node path accesses
only the cut counter and cyclic size on the stack, as the SDK does. Native
objects also assemble for x86-64 Windows MSVC and macOS; Windows runtime
execution was not performed. Non-x86-64 and Miri use the portable Rust path.

## Encoder and short-match mixer

The encoder has const-generic checked and unchecked window readers, selected
once per coding block by `LZMA_TURBO_ENCODER=checked|unchecked` with
`kernel-ab`. The checked path remains the differential oracle. Without that
feature there is no environment lookup or selection branch.

The optimal-state table uses the SDK's transition margin instead of repeated
index checks: every transition is below `K_NUM_OPTS`, and predecessor and
reset accesses remain within those initialized states. The finder variant
is selected once per coding block. The four initial repetition candidates
are expanded in SDK order, including the full-length early exit.

`MixMatches2/3/4` are specialized by hash kind, as the SDK's selected function
is. Table, history, lookahead, and output bounds are established once per
call; masked hash entries and window/output accesses then use pointers.
Inlining the mixer into `GetMatches` avoids rebuilding the same metadata.
The finder now supplies its already-established hash/history/lookahead
margin to the mixer, with the checked entry retained for other calls and
differential testing. Parser match-pair reads similarly use the finder's
bounded, initialized pair count. `GetPrice_PureRep` is forced inline.

The MT allocation pointer is cached for one coding block; current offsets
and availability are refreshed after finder advancement. Each byte borrow
still lasts for only one parser position. Window slides move bytes within
the same allocation; create/direct-block replacement happens outside the
coding block. The checked parser retains its ordinary slice accesses.

`LitEnc_Encode` and `LitEnc_EncodeMatched` now keep `range` local across
their bit updates, as the SDK does. `ShiftLow` only changes low/output state;
the literal routine writes range back on completion. Coding arithmetic and
probability updates are unchanged. A stack binding also gives `ShiftLow` one
stable pointer to the borrowed output-stream handle across a literal's bits,
matching the SDK's stream-handle shape. These range-coder changes add no
unsafe code or allocations.

## Measurement method

The historical rows below are three interleaved runs, with order reversed on the
middle repeat. Ubuntu 24.04, Linux 7.0.0-1014-aws, Rust 1.97.1, the profiling
profile (release optimization, fat LTO, one codegen unit, panic abort, symbols
retained). Those baseline and candidate builds enable `std,enc,kernel-ab` without
default features and explicitly select `LZMA_TURBO_TREE_KERNEL=reg`.

The c7a.xlarge exposes four Zen 4 cores (EPYC 9R14); c7i.xlarge exposes two
Sapphire Rapids cores with SMT (Xeon Platinum 8488C). No benchmark processes
overlapped on either instance. Single-thread runs are pinned to logical CPU 0.
SDK C is the repository's `lzma-oracle`, built by `cargo xtask lzma-util`
with `cc -O2 -DZ7_ST -D_7ZIP_ST`; 7-Zip is the pinned official 26.03 binary.

The probe is [encoder-perf-probe.rs](../bench/encoder-perf-probe.rs). It uses
SDK level 5, not the XZ preset mapping used by `lzma-bench --shot encode`.
The macro compares raw LZMA2 output from the library against 7-Zip's XZ
container, with filters disabled. This includes container/CRC work only on
the reference side. Both wall measurements cover the entire process;
the Rust input and output are buffered. These are encoder comparisons, not
a rerun of the separate decoder/container acceptance matrix.

The current timing round instead uses shipping `std,enc,asm` features,
without `kernel-ab` or native-CPU compiler flags. GNU time records elapsed,
user, system and maximum resident KiB; a monotonic timer measures the whole
child process. No sampling is active during acceptance measurements.
The additional [streaming probe](../bench/encoder-stream-probe.rs) reads a
file and writes to `/dev/null`, matching the reference's I/O pattern. The
buffered probe keeps the complete input and compressed output in memory;
its timing and RSS results are reported separately, not replaced by the
streaming rows.

Tree counts use `perf record -e instructions:u -c 100000`, never frequency
mode. Every accepted recording has zero throttle and lost-event records.
Initial recordings with kernel throttling are excluded. The disposable
hosts use `perf_cpu_time_max_percent=0`; set `perf_event_max_sample_rate=100000`
before disabling that limit. Counts are samples times 100,000, with the
usual partial-period uncertainty. Wall-time medians come from separate
unsampled `perf stat -e instructions:u,cycles:u` runs.

Inputs are prefixes of the current seeded fixture generator:

| Input | SHA-256 |
| --- | --- |
| 16 MiB | `b8c9ecc45cbeaa336295085b166aaa8fc8917d69c8ca203512f075bf39049d86` |
| 48 MiB | `b5f980c0328566e802a38e6c6fde838cb655276cfaf6b6b0b32143fdf850c800` |
| 256 MiB | `1f01b30af4ef1142dc8fc0c9f7298028d029ea163b385e9d87b8ef15079498a6` |

## Final tree diagnostics

Three interleaved fixed-period `instructions:u -c 100000` recordings on the
48 MiB T2 row, separate from acceptance timing. All accepted recordings have
zero throttle, unthrottle and lost-event records. The first AMD recording
was throttled and excluded; the repeat passed after resetting perf's sampling
controls. Both final dependent node paths retain `son` in r9 and have no
stack operands at offsets 0x120–0x168.

| CPU | Final BT thread instructions | Official 7-Zip | Difference |
| --- | ---: | ---: | ---: |
| Zen 4 | 6.0507G | 6.6143G | -8.52% |
| Sapphire Rapids | 6.0506G | 6.6142G | -8.52% |

Both meet the original 6.79G BT ceiling. Per-run process wall/CPU/RSS and
zero-loss audits are retained in the JSON as diagnostics. Sampling changes
timing, so those times are not used for the wall/CPU gate.

## Historical tree and macro results

48 MiB, level 5, two finder threads; billions of tree-thread instructions:

| CPU | Baseline | Candidate | Official 7-Zip | Reduction from baseline |
| --- | ---: | ---: | ---: | ---: |
| Zen 4 | 8.7035 | 6.6168 | 6.6143 | 23.98% |
| Sapphire Rapids | 8.7031 | 6.6165 | 6.6141 | 23.98% |

256 MiB, level 5; median process wall seconds. Ratio is reference/candidate;
above 1 means the candidate is faster than 7-Zip.

| CPU | Total thread budget | Baseline | Candidate | 7-Zip | Ratio | Time reduction |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Zen 4 | 2 | 30.977 | 27.012 | 26.519 | 0.982 | 12.80% |
| Zen 4 | 4 | 16.872 | 14.614 | 13.715 | 0.938 | 13.38% |
| Sapphire Rapids | 2 | 33.612 | 28.907 | 27.703 | 0.958 | 14.00% |
| Sapphire Rapids | 4 | 22.005 | 19.152 | 18.126 | 0.946 | 12.97% |

The candidate preserves the baseline's compressed sizes: 234,466,839 raw
bytes at T2 and 234,495,532 at T4. The SDK's block splitting differs between
those thread budgets.

## Verified single-thread encoder results

16 MiB, level 5, one thread on CPU 0. Instructions and cycles are billions,
counted with `perf stat`; medians of three interleaved runs. The checked arm
uses the retained tree and checks every parser-window and low-hash access.

| CPU | Build | Instructions | Cycles |
| --- | --- | ---: | ---: |
| Zen 4 | Baseline | 16.5866 | 8.1610 |
| Zen 4 | Candidate, checked encoder | 15.5660 | 7.4225 |
| Zen 4 | Candidate, unchecked encoder | 14.4124 | 7.2331 |
| Zen 4 | SDK C | 12.8399 | 16.2502 |
| Sapphire Rapids | Baseline | 16.5831 | 8.5040 |
| Sapphire Rapids | Candidate, checked encoder | 15.5626 | 7.6148 |
| Sapphire Rapids | Candidate, unchecked encoder | 14.4090 | 7.4428 |
| Sapphire Rapids | SDK C | 12.8362 | 17.2392 |

The candidate saves 13.11% of instructions on both CPUs. Median cycles fall
11.37% on Zen 4 and 12.48% on Sapphire Rapids. Rust still executes 12.25%
more instructions than this SDK C build, although it uses fewer cycles.
The within-5% instruction target is not met. Named exception: T1 instruction count is diagnostic, not an acceptance gate, following the maintainer’s instruction to judge wall time and CPU time instead. The +12.25% result is retained; no instruction-parity claim is made.

All timed Rust arms report 14,667,336 output bytes on the 16 MiB input.
The final checked and unchecked candidates match the C oracle byte for byte
on both processors, after validating and removing its 13-byte LZMA-Alone
header. Raw output SHA-256
`756d5c62eaa9f9170c70518c26d041b8c1a2d0875fcf3a1418839983e3d21f26`.

## Correctness evidence

- Full native workspace suite with `kernel-ab`, required pinned SDK oracles,
  register tree, and unchecked encoder: 370 passed, zero failed, two existing
  ignored tests; `--no-fail-fast` covered 32 test targets.
- Required release SDK parity plus golden suites: 22 passed. The checked encoder
  also passed the golden corpus.
- Register-tree differential tests compare every output and tree write,
  answer, and refusal. Cases include direct/cyclic windows, short output,
  exact capacity, long-match copying, invalid children, wrap boundaries, and
  zero cut depth. The final debug-only zero-cut guard passed targeted debug
  and release tests on both x86-64 processors and locally. Its addition left
  the entire measured release `.text` byte-identical on both processors.
- The bounded word comparison additionally tests every byte alignment and
  mismatch position around 8-, 16- and 32-byte boundaries, both ordering
  directions, and an allocation ending exactly at the match limit. All 11
  final native tree/mixer/encoder differential tests pass on both CPUs.
- Encoder differential tests compare complete checked/unchecked output for
  both parsers, finder families, inline/threaded execution, tiny tails,
  dictionary wrap, noise, dense small alphabets, and repeated bytes.
- Low-hash differential tests compare return values, every output word, and
  the entire mutated hash table for all three mixer kinds.
- Native release differential tests passed on both x86-64 CPUs. The existing
  required Miri job includes tree/mixer tests through `the_` and explicitly
  includes the encoder differential test. Miri was not run locally; it
  verifies the portable Rust path, not the native assembly.
- Formatting, workspace/all-target Clippy with and without `kernel-ab`,
  enc-only, no-default-features, and wasm32 bare/host-hook checks passed.
- For the current buffered block-destination candidate on both CPUs, complete 256 MiB
  T2 and T4 output matches the previous build and the official 7-Zip XZ
  blocks' LZMA2 payload byte for byte. Both outputs decode back to the input.
  T2 is 234466839 bytes, SHA-256
  `c2cdaa999fec4523b4183317a74400ca5a61cd5a5b4a8d2e2e4a83f5fbf65a43`;
  T4 is 234495532 bytes, SHA-256
  `0d292910c6b5a831e0f29808b54fa213fc2c17ff60a605d61afa013022caa8a7`.
- Ownership tests verify pointer/capacity transfer, ordered appending,
  generic sink buffer reuse, and output parity across empty input, chunk
  boundaries, dictionary wrap and encoder reuse. Both focused tests also
  passed on the two native benchmark processors.
- Current block-destination tests cover exact and partial blocks, five or
  more blocks, empty input, noise, repeated bytes, encoder reuse, one- and
  two-thread finders, and checked rejection of writes past a region's end.

Individual accepted measurements and source fingerprints are in
[encoder-perf-0.9.0.json](encoder-perf-0.9.0.json). Full logs, source experiments,
symbol reports, and native object disassembly are under ignored `target/`.
Large perf recordings were audited remotely; durable results retain sample
counts and zero-throttle/loss audits, not the recordings themselves.

## Rejected experiments

Byte-at-a-time tree extension, forcing the old mixer inline, forcing
`GetOptimum` out of line, and extra state/probability-table unchecked accesses
did not improve elapsed time. Splitting range-output state gave no additional
instruction saving. They are not retained. The positive-length Rust tree
experiments were superseded by the measured SDK assembly port.

The final wall/CPU experiments also rejected earlier word-scan entry, four-
and two-byte tail loads, 64-byte label alignment, extra hash/probability
unchecked accesses, and padding between thread states. None consistently
improved both processors. The retained combination uses the bounded 16/8-byte
scan and safe borrowed output-handle binding. The prior eight-byte-only
candidate passed all wall rows but missed the Sapphire Rapids T4 CPU gate
(+2.32%); its seven-run rows remain in the JSON report.

Forcing `ReadMatchDistances` and `GetMatches` inline reduced single-thread
instructions from 14.4121G to 14.0229G on Zen 4 and from 14.4086G to 14.0194G
on Sapphire Rapids. However, the latter's 48 MiB threaded median encode time
rose from 4.318s to 4.452s. Isolating `ReadMatchDistances` alone still increased
that threaded median from 4.102s to 4.239s in a separate three-run comparison.
These experiments are rejected for the throughput regression. Their raw rows
are retained in the JSON report; these small-row encode times are not the
256 MiB whole-process acceptance numbers above.

## Reproduction

Copy `bench/encoder-stream-probe.rs` into `examples/perf_probe.rs` in an
isolated checkout, then build with the measured shipping features:

```sh
rtk proxy cargo build --locked --profile profiling --no-default-features \
  --features std,enc,asm --example perf_probe
rtk proxy cargo run --locked -p xtask -- sevenzip
rtk proxy cargo run --locked -p xtask -- lzma-util
```

Regenerate the seeded fixture with
`rtk proxy cargo run --locked -p xtask -- fixtures`, and take its first 16,
48 and 256 MiB. Verify the digests above. For the final gate, run seven
interleaved repeats, alternating order each repeat. Repeat with thread
budgets 2 and 4 on each CPU. Do not sample during these runs:

```sh
rtk proxy /usr/bin/time -f 'wall=%e user=%U system=%S rss_kib=%M' \
  target/profiling/examples/perf_probe 2 4 p256.bin > /dev/null
rtk proxy /usr/bin/time -f 'wall=%e user=%U system=%S rss_kib=%M' \
  target/sevenzip/7zz a -txz -mx5 -mmt4 -mf=off \
  -so unused.xz p256.bin > /dev/null
```

Record monotonic whole-child-process wall time as well. CPU time is the sum
of user and system time; RSS is GNU time's process maximum in KiB. Report
medians for all three metrics. To reproduce the final buffered API rows,
build the other probe with identical features and use seven interleaved runs,
including the previous buffered build as a third arm.

The following are historical counter diagnostics, performed separately from
acceptance timing, using `bench/encoder-perf-probe.rs` copied into the same
example. The checked/unchecked comparison requires rebuilding with
`std,enc,kernel-ab`; use three interleaved repeats, reversing the middle one:

```sh
rtk proxy env LZMA_TURBO_TREE_KERNEL=reg perf stat \
  -e instructions:u,cycles:u taskset -c 0 \
  target/profiling/examples/perf_probe 1 1 p16.bin
rtk proxy env LZMA_TURBO_TREE_KERNEL=reg perf record \
  -m 1024 -e instructions:u -c 100000 \
  target/profiling/examples/perf_probe 2 2 p48.bin
rtk proxy env LZMA_TURBO_TREE_KERNEL=reg perf stat \
  -e instructions:u,cycles:u \
  target/profiling/examples/perf_probe 2 4 p256.bin
rtk proxy perf stat -e instructions:u,cycles:u \
  target/sevenzip/7zz a -txz -mx5 -mmt4 -mf=off \
  -so unused.xz p256.bin > /dev/null
```

Repeat the macro with total thread budget 2. The reference single-thread
command is `lzma-oracle 5 1 4 3 0 2 32 33554432 p16.bin /dev/null`.
Set `LZMA_TURBO_ENCODER=checked` for the candidate's checked arm. Do not use
the perf-record elapsed times as throughput results, and reject recordings
containing `PERF_RECORD_THROTTLE` or `PERF_RECORD_LOST`.

## EC2 cleanup

All sixteen instances used across these rounds are confirmed terminated after
explicit cleanup or the recorded service-initiated interruptions. Region: benchmark-region; spot, SSM only,
32 GiB gp3 root disks deleted on termination. The earlier round included a
Sapphire Rapids spot interruption.

| Instance role | Launch UTC, 2026-10-11 | Termination initiated UTC | Instance-hours |
| --- | --- | --- | ---: |
| Zen 4, earlier round | 01:24:57 | 01:54:05 | 0.48556 |
| Sapphire Rapids, interrupted | 01:25:01 | 01:35:45 | 0.17889 |
| Sapphire Rapids, replacement | 01:38:16 | 01:56:12 | 0.29889 |
| Zen 4, final round | 02:37:03 | 03:51:58 | 1.24884 |
| Sapphire Rapids, final round | 03:11:35 | 03:51:58 | 0.67328 |
| Zen 4, wall/CPU round | 03:57:12 | 07:07:48 | 3.17679 |
| Sapphire Rapids, wall/CPU round | 03:57:14 | 07:07:48 | 3.17623 |
| Zen 4, buffered RSS round | 12:55:51 | 13:38:53 | 0.71745 |
| Sapphire Rapids, buffered RSS round | 12:55:54 | 13:38:53 | 0.71661 |
| Zen 4, unmeasured layout round | 13:48:32 | 14:00:17 | 0.19608 |
| Sapphire Rapids, unmeasured layout round | 13:48:38 | 14:00:17 | 0.19442 |
| Sapphire Rapids follow-up | 14:06:26 | 14:38:15 | 0.53028 |
| Zen 4 follow-up | 14:06:24 | 14:43:29.221604 | 0.61812 |
| Zen 4 follow-up | 14:55:37 | 15:09:56 | 0.23861 |
| Sapphire Rapids follow-up | 14:43:07 | 16:51:35.374784 | 2.14122 |
| Zen 4 follow-up | 15:11:07 | 16:51:35.374784 | 1.67455 |
| Total | | | 16.26580 |

Hours use launch-to-termination-initiation timestamps, not a billing claim.
The fifth round installed compiler utilities but transferred no source and
ran no benchmarks; its instances were terminated while source export awaited
approval.

The SPR recovery host was reused for T2 and counted once. The interrupted initial T2 AMD host is included through its service-initiated termination time. The final two hosts were terminated by explicit instance ID after results were archived.

## Acceptance limits

The final BT walk meets the requested 6.79G instruction ceiling on both
CPUs and uses about 8.52% fewer BT instructions than the fresh official
binary. Its dependent node path has the requested spill-free shape. The
matched file-to-file seven-run matrix meets the original wall and total CPU
target of at most 2% above SDK on both processors and thread budgets.
The buffered follow-up uses the operator's revised CPU ceiling of 3%, with
wall time and peak RSS recorded throughout. Its own measurements determine
acceptance; it does not inherit the streaming result. These measurements
cover the stated corpus and configuration, not every input, preset or
processor.

Signed local implementation commits have been created. The provisional package bump to 0.9.1 has been corrected to the maintainer’s 0.9.0 target. No push, new PR, release tag or publication is part of this work. The existing PR remains the integration target; publishing is an operator action.
