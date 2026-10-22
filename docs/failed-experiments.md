# Failed experiments

What has been tried in this crate and did not pay: slower, no gain, or a
gain that cost more than it saved. Read it before starting performance work,
and do not retry an entry without new evidence: a different workload, a
different machine class, or the change the entry says would have to come
first. Each entry names its source, where the full numbers are.

A new failed experiment is added here in the same change that records it.
"Slower" means more time; percentages are against the build the source
compared with.

## Decoder hot loop

- **Range-coder bit selects written as `if`/`else`.** 36.9 MiB/s, below the
  original branchy code (42.4) and the `u32` mask form (50.1): LLVM rebuilds
  the branch. Selecting the prefetched child as `u16` or with an `if` lost
  2-5 MiB/s against the mask form. `docs/perf-log.md:81`, commit `f047fcd`.
- **Branchless length, pos-slot, align and reverse trees.** No measurable gain
  on a literal-heavy payload (50.1 vs 48.1 MiB/s, inside the noise). Kept for
  shape, not speed. `docs/perf-log.md:88`.
- **arm64 asm restructuring (M5 Max).** Five byte-exact rewrites of
  `LzmaDecOpt.S` were all slower or in the noise; the stock loop is the local
  optimum and the crate's `asm!` stays a faithful translation.
  `docs/arm64-tuning.md`, `asm-lab/RESULTS.md`.
  - Carrying `range >> 11` with `msub` for the remainder: +2.2%. The
    isolated recurrence got shorter (6.30 to 5.53 cycles) but each bit cost two
    more instructions. This retires the "shorten the recurrence" family.
  - The same plus precomputed child offsets: +2.8%, with spills, because
    register pressure is at its limit.
  - 32-bit probabilities with a 64-bit child-pair load: no consistent sign
    (+0.5% to +4.5%, -1.7% at 1 GiB). Not adopted: it changes the ABI-visible
    layout and doubles probability memory.
  - Branchless renormalisation: +9.1%, the worst. The renorm branch predicts
    well, and the change reads a byte past `bufLimit`.
  - Copy-loop build flags on a match-heavy stream: no effect, or marginally
    worse.
  - Rejected before building: 32-bit renormalisation (breaks drop-in
    `LzmaDec_TryDummy`; worth revisiting only in a decoder that owns both
    sides), two tree levels per addressing op (`ldp` has no register-indexed
    form), a fused renorm test, and a two-op probability update (wrong when
    `p % 32 == 0`).

## Multi-threaded decoder

- **Letting the chase stand aside for a worker, and nothing more.** Fed in
  pieces smaller than a run, the chase took every run and the decode never
  threaded: 23.9 s at eight threads against the ring's 8.2 s. On a stream
  already on disk the chase cost 1.50x. `docs/porting.md` ("The chase
  decoder"), `CHANGELOG.md:1368`.
- **Checksumming as the consumer drains.** 16% of an eight-thread decode,
  because it runs in the ring's serialised section. Checksums are taken on the
  workers instead. `CHANGELOG.md:1440`.
- **Allocating the ring with calloc semantics.** 2.336 s before and after at
  eight threads; reverted, because it added an `unsafe` block for nothing.
  `docs/perf-log.md:439`.
- **An input-buffer budget of half of what the rest is not holding.** 60%
  slower than no limit at a 512 MiB limit. Commit `8e7cf15`.
- **Sizing each copy to whatever room is free.** Allocator churn, and slower
  than the version before. Commit `79457a6`.
- **Probing an ordinary reader into the bare serial coder.** Lost live
  progress reports and failed three tests. `docs/mac-fleet-audit.md:2014`.
- **Rewriting the non-solid extraction queue.** Not supported: 97% of samples
  are in the decoder asm. `docs/mac-fleet-audit.md:1170`.
- **A pointer-cursor port of `Bcj2Dec_Decode`.** Converter cycles fell from
  223M to 212M against 2.73G cycles of LZMA decoding: -0.26% overall, within
  noise on every lane. It stays behind `kernel-ab` (`LZMA_TURBO_BCJ2=sdk`).
  `docs/mac-fleet-audit.md:3256`.
- **Borrowed delivery on the non-solid single-thread path.** About 0.7%
  faster, but streaming RSS went over the regression limit (+15.58% against
  the SDK). `docs/mac-fleet-audit.md`, borrowed-delivery section.

## Encoder and match finder

- **Fixed-count literal loops.** Every variant of the eight-way tree, mixer
  and literal-loop comparison failed a gate. They missed Intel T4 CPU by
  3.45-4.52%, and one missed buffered-zeros T2 wall by 8.10% despite lower
  CPU. `docs/encoder-perf-0.9.0.md:193`.
- **Lower-RSS T2 paths** (direct input, single-block worker). About 50 MiB
  saved for more CPU. `docs/encoder-perf-0.9.0.md:504`.
- **Codegen tweaks with no elapsed-time gain.** Byte-at-a-time tree
  extension, forcing the old mixer inline, forcing `GetOptimum` out of line,
  extra unchecked state and probability-table accesses, and splitting the
  range-output state. `docs/encoder-perf-0.9.0.md:745`.
- **Scan and layout tweaks, none better on both CPUs.** An earlier word-scan
  entry, 4- and 2-byte tail loads, 64-byte label alignment, extra unchecked
  hash and probability accesses, and padding between thread states. An
  8-byte-only scan missed SPR T4 CPU by 2.32%. `docs/encoder-perf-0.9.0.md:751`
  and `:360`.
- **Forcing `ReadMatchDistances` and `GetMatches` inline.** Fewer T1
  instructions (14.41G to 14.02G), but the SPR 48 MiB threaded encode went
  from 4.318 to 4.452 s. `docs/encoder-perf-0.9.0.md:759`.
- **Separating the hash, tree and LZ state by 128-byte lines, first attempt.**
  Mac wall +2.63%, x86 T8 wall +2.23%. A later isolation port did pay and
  ships, so this entry rules out that first layout, not the idea.
  `docs/mac-fleet-audit.md:1261`, against `:2476`.
- **Copying a match pair as one checked slice.** No useful gain.
  `docs/mac-fleet-audit.md:1277`.
- **Rewriting the consumer's pair copy.** Not justified: the copy is about
  0.43% of the encode. `docs/mac-fleet-audit.md:3077`.
- **Reordering the low-hash loads.** x86 full-thread wall +4.3%, AES +5.5%.
  `docs/mac-fleet-audit.md:1011`.
- **The SDK range-local port.** L5 T4 +5.48% wall and zeros T2 +4.40% against
  the product before it. `docs/mac-fleet-audit.md:3487`.
- **The C-shaped ARM64 SDK tree walk** (`sdk-arm64`, `sdk-arm64-wide`). Its
  solid-tree gain does not pay for its T2 regression; it stays diagnostic. The
  negative-offset port is what ships. `docs/mac-fleet-audit.md:4844`,
  `src/kernel_ab.rs`.
- **The combined encoder allocation cohort.** Against table-only allocation:
  T4 +5.22% wall, T2 +3.49%, L1 and L3 T16 about +5%.
  `docs/mac-fleet-audit.md:2854`.
- **An aligned allocator shim for input and rings.** Up to +1.15% CPU and
  +1.88% wall. `docs/mac-fleet-audit.md:2807`.
- **RSS reductions that did not reduce RSS.** Moving worker-table
  construction to the caller, capacity rounding, early completion collection,
  reduced read-ahead, a compact folder representation, removing the spare
  spill slots, removing huge-page advice. `docs/mac-fleet-audit.md:1001`,
  `:1126`, `:1496`, `:601`, `:1775`.
- **Ordered-output memory.** `MADV_ZERO` reclaim saved no median RSS, and a
  bounded one-spare transfer left RSS 4.83% over the reference; both were
  removed. `docs/mac-fleet-audit.md:4960`, `:4984`.
- **xz multi-threaded direct delivery alone.** Saved 42-84 MiB, but T4 was
  3.70% slower than the product before it; fixed-buffer reuse replaced it.
  `docs/mac-fleet-audit.md`, xz MT section.

Two first attempts lost and were later made to pay in another form, so they
are not grounds for refusing the idea: the full SDK x86 tree walk (first
cohort +9-12% at T2-T8, now the shipping `sdk` walk in its shared-cursor form,
`docs/mac-fleet-audit.md:2947`), and the portable ARM register walk (first
+5.9% on zeros L5 T2, now enabled with that trade-off stated,
`docs/mac-fleet-audit.md:940`).

## BCJ2 encoder

- **Running the main LZMA2 coder as a pipeline stage.** T2 wall -10.55%, but
  in a 15-run control T2 regressed about 6% and T4 CPU and RSS rose 8.70% and
  10.54%. Removed. `docs/mac-fleet-audit.md:1673`.
- **Per-pipe notification.** T16 +13.65% wall, +29.48% CPU. It kept the
  global mutex, so it was not a full independent-binder port.
  `docs/mac-fleet-audit.md:1723`.
- **A 512 KiB handoff piece.** T2 +3.74% and T8 +4.87%. A size-only retry has
  no new evidence behind it. `docs/mac-fleet-audit.md:2243`.
- **An independent-binder rewrite.** Not justified by the source or the
  profile. `docs/mac-fleet-audit.md:3135`.

## SIMD and asm kernels

- **16-byte match extension.** Wins the scan alone (about 3x) and loses the
  encode (-1.20% at p6, -0.75% at p9 throughput): most runs are shorter than a
  word. `run16` stays behind `kernel-ab`. `docs/simd-kernels-report.md:136`.
- **A BCJ ARM64 scan.** +1.2-2.3%, below the 2% gate, on a filter that is 4%
  of the lane. Removed. `docs/simd-kernels-report.md:189`.
- **SIMD `normalize3`.** It already auto-vectorises.
  `docs/simd-kernels-report.md:254`.
- **A threaded match-finder hash thread.** The hash thread spends 93% of its
  samples blocked in `Semaphore::wait`; the ceiling is zero until the
  handshake changes. `docs/simd-kernels-report.md:262`, commit `76bee65`.
- **SDK normalisation kernels** (SSE4.1, AVX2, NEON). 20-66% faster in
  isolation, +0.37% wall end to end. Kept without a speed claim.
  `docs/mac-fleet-audit.md:3219`.

## CRC and crypto

- **Checksum feature flags.** crc-fast's `optimize_crc32_*` and `vpclmulqdq`
  features are vestigial, and sha2 0.11 has no asm feature.
  `docs/perf-log.md:352`, `:395`.
- **RustCrypto bulk CBC.** Stored and encrypted archives 42-45% slower on the
  Mac, x86 streaming +3.3%. Removed. `docs/mac-fleet-audit.md:1861`.
- **1 MiB AES batches instead of 64 KiB.** No throughput gain, more RSS.
  `docs/mac-fleet-audit.md:1847`.
- **Letting the AES buffer grow to a finite budget.** +1.34% faster for +24.10%
  RSS. `docs/mac-fleet-audit.md:2119`.
- **The EVP streaming CBC interface, or another cipher backend.** No extra
  hardware kernel, it needs PKCS7, and CBC is about 0.18 of 110 CPU-seconds.
  `docs/mac-fleet-audit.md:1823`.

## Build profile

- **`opt-level = 3` in the test profile**, for the crate or for crc-fast,
  sha2 and aws-lc. Tests ran no faster, and cold builds took up to 10 s
  longer. Measured on a loaded machine. Commit `970cc85`,
  `CHANGELOG.md:152`.

## Measurement traps

Traps that once made a result read as a flat zero or a false win.

- `cmd.exe`'s `set VAR=value && prog` puts a trailing space in the value, so
  both arms of an A/B ran the same code. `docs/simd-kernels-report.md:59`.
- A contended M5 read -2.27% on a lane that was +0.27% when quiet.
  `docs/simd-kernels-report.md:304`.
- Windows `GetProcessTimes` reported +8.76% CPU where scheduled CPU was
  -0.06%, and it undercounts BCJ2. `docs/mac-fleet-audit.md:432`, `:3143`.
- A Python launcher carried its own RSS into a child's peak (281 MB for a
  trivial child). `docs/mac-fleet-audit.md:1352`.
- xz auto blocks with `-mmt1` still ran two-thread LZMA.
  `docs/encoder-perf-0.9.0.md:199`.
