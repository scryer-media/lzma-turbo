# Decode throughput log

Every entry is a measurement of `lzma-turbo` against the oracles on the same
machine in the same session, with the commit the number belongs to. See
[benchmarking.md](benchmarking.md) for the oracles and
[porting.md](porting.md) for the acceptance gate: single-threaded decode
within 3% of `7zz t -mmt=1`.

Machine: Apple M5 Max (arm64), 18 cores, macOS. `7zz` 26.01, `xz` 5.x, and
`7lzma` built from the reference tree at 0766b733 with its own `makefile`
(the portable C `LzmaDec_DecodeReal`, no assembly).

Fixtures (gitignored, see `bench/fixtures/README.md`): `p256.bin.lzma` is 256
MiB of synthetic payload at compression ratio 0.88, LZMA1, 8 MiB dictionary;
`payload.bin.lzma` is the same payload at 1 GiB; `p256.bin.xz` is the 256 MiB
payload as LZMA2 in an xz container.

## A note on the machine

This machine is shared with other build and test workloads, and the load
average during this session ranged from about 2 to 16 on 18 cores. Numbers
taken under load are pessimistic and noisy in one direction only: contention
can slow a run down but never speed it up. Two consequences, both applied
below.

- The harness reports the **median of N** runs per decoder, which is what the
  brief asks for; when the load moved between two decoders' blocks the median
  understates the faster one. Each table therefore also says what the load was
  doing.
- While iterating on the decoder itself, the figure used to accept or reject a
  change is the **best of 7** runs in one process, which is the run least
  disturbed by other work and the only statistic that is stable enough to
  resolve a 3% change on this machine.

## Baseline: the faithful C port

Commit `8265abd` (`feat(lzma): port the 7-Zip single-threaded LZMA and LZMA2
decoder`), `cargo run --release -p lzma-bench -- --runs 3`, load average
around 3.

| fixture | ours | `7lzma d` (C) | `7zz t -mmt=1` (asm) | `xz -T1` | vs 7lzma | vs 7zz |
| --- | --- | --- | --- | --- | --- | --- |
| `p256.bin.lzma` | 6.393 s | 6.299 s | 4.150 s | 6.172 s | 1.015 | **1.540** |
| `payload.bin.lzma` | 33.078 s | 24.195 s | 26.197 s | 31.197 s | 1.367 | **1.263** |
| `p256.bin.xz` (LZMA2) | 6.036 s | n/a | 3.894 s | 6.376 s | n/a | **1.550** |

Reading: the port is at parity with the reference C decoder on the 256 MiB
LZMA1 fixture (1.5% behind), which is the first milestone in the brief. It is
half again slower than `7zz`, and `7zz` is exactly the same C decoder with
`Z7_LZMA_DEC_OPT` on, i.e. with `Asm/arm64/LzmaDecOpt.S` substituted for
`LzmaDec_DecodeReal_3`. The whole remaining gap is that assembly loop.

The `payload.bin.lzma` row is the one contended measurement in this table: a
stray debug test binary from an earlier session was pinned to a core for its
whole duration (it could not be killed from this session), and four `rustc`
processes started during it. Decoding only the first 256 MiB of
`payload.bin.lzma` runs at the same rate as `p256.bin.lzma`, which is the same
data, so the 1.37 figure is measurement noise rather than a size effect.

## Step 1: branchless bit decoding in the portable loop

Best of 7 on `p256.bin.lzma`: **42.4 -> 50.1 MiB/s**, a 18% gain, and the
first change that moves the portable path past the reference C decoder.

`LzmaDec_DecodeReal`'s `IF_BIT_0` / `UPDATE_0` / `UPDATE_1` is a branch on a
range-coder bit, which is close to a coin flip and mispredicts about half the
time — eight times per literal, and literals are almost all of this payload.
`Asm/arm64/LzmaDecOpt.S` does not branch there: `NORM_CALC` + `CMOV_range` +
`CMOV_code_Model_Pre` + `PUP_BASE_2` decode the bit with conditional selects,
and `BIT_1_R` loads *both* children of the tree node before the select, so the
next level's load latency overlaps this level's arithmetic.

The portable loop now has the same shape, in Rust:

- one `kBitModelOffset` expression serves as both `UPDATE_0` and `UPDATE_1`
  (`ttt - (t >> kNumMoveBits)` with `t = bit ? ttt : ttt - kBitModelOffset`);
- `range`, `code`, the symbol and the next probability are selected with
  arithmetic masks, not `if`;
- the literal, length, pos-slot and align trees prefetch both children.

Two things had to be measured rather than assumed. Writing the selects as
`if cond { a } else { b }` is *slower than the original branchy code* (36.9
MiB/s): LLVM re-forms a conditional branch out of arms this cheap, so it pays
the mispredict and the extra work. Written as mask arithmetic, LLVM emits
`csel` and the branch is gone. Selecting the prefetched child as a `u16` or
with an `if` also lost 2-5 MiB/s against the `u32` mask form.

The length, pos-slot, align and reverse-bit trees got the same treatment in
the same step; on their own they were worth nothing measurable on this payload
(50.1 vs 48.1 MiB/s is inside the noise), because a payload that compresses to
0.88 is nearly all literals. They are kept for shape consistency and for
match-heavy inputs.

## Step 2: the SDK's own assembly loop

Commit `da2e5dd` (`feat(lzma): decode with the SDK's hand-written aarch64 and
x86_64 loops`),
`cargo run --release -p lzma-bench -- --runs 5`, load average around 3.

The harness changed with this step: it now **interleaves** decoders, running
one timed run of each per round instead of all N runs of one decoder before
moving to the next. On a shared machine a block schedule gives whichever
decoder happened to run during a quiet minute an advantage that no number of
repetitions removes, because the median is taken within the block. Round-robin
spreads the same load over every decoder. This is why the `7zz` column moves
between the tables below and the ones above.

| fixture | ours (asm) | ours (portable) | `7lzma d` (C) | `7zz t -mmt=1` (asm) | `xz -T1` | asm vs 7zz |
| --- | --- | --- | --- | --- | --- | --- |
| `p256.bin.lzma` | 3.746 s | 5.126 s | 5.680 s | 3.771 s | 5.868 s | **0.993** |
| `payload.bin.lzma` | 15.248 s | 20.376 s | 22.726 s | 15.032 s | 23.339 s | **1.014** |
| `p256.bin.xz` (LZMA2) | 3.748 s | 5.109 s | n/a | 3.834 s | 6.059 s | **0.978** |

That is the acceptance gate: 0.978, 0.993 and 1.014 against `7zz t -mmt=1`,
all inside 3%, two of the three faster than 7-Zip. The portable loop is at
0.90 of the reference C decoder and 1.36 of `7zz`; the difference between the
two columns is entirely the inner loop.

Whether the arm64 loop could be beaten on this machine was asked separately
and answered no: see [arm64-tuning.md](arm64-tuning.md), which measures five
structural rewrites of the loop (all lost or were noise), and derives the
~6.3-cycle loop-carried recurrence the stock loop already runs at. That is why
the aarch64 module here is a faithful translation of the stock `.S` and not a
tuned variant, and why "do not add an instruction to the hot path" is the rule
the translation was checked against.

### What was ported

`Asm/arm64/LzmaDecOpt.S` and `Asm/x86/LzmaDecOpt.asm` (the latter with
`Asm/x86/7zAsm.asm` spliced in), translated line by line into
`src/lzma/decode_opt/lzma_dec_opt_aarch64.S`,
`lzma_dec_opt_x86_64_sysv.S` and `lzma_dec_opt_x86_64_win64.S`, each included
verbatim into a `#[unsafe(naked)]` Rust function by
`decode_opt/{aarch64,x86_64}.rs`. A naked function is the right vehicle: these
are whole C-ABI functions with their own prologue, frame and callee-saved
register handling, so there is nothing for the register allocator to do and
nothing to declare but the symbol.

The assembly reads `CLzmaDec` through hardcoded offsets, so `AsmLzmaDec` is a
`#[repr(C)]` mirror of it whose every offset and whose total size are asserted
at compile time with `offset_of!`. Nothing about the Rust state layout can
drift away from the assembly without failing the build.

Three mechanical differences from the reference files, listed in each file's
header and nowhere else:

- the C preprocessor and the MASM macro assembler are resolved by hand, for
  the same configuration the reference builds use (16-bit probabilities,
  `LZMA_USE_4BYTES_FILL` on, `_LZMA_SIZE_OPT` off);
- every label gains an `L` prefix, because Mach-O refuses a conditional branch
  to a label that is not assembler-local;
- on x86, every constant expression is folded to a plain integer.

That last one is not cosmetic. LLVM's GNU-Intel parser silently drops the
index register of a memory operand whose scale is parenthesised:
`[probs + sym * (2)]` assembles as `[probs]`, with no diagnostic. The first
translation had that in every literal tree load, which is exactly the sort of
bug a differential test catches and a reading does not.

### How the translation was checked

Beyond the differential tests, the x86 translation was checked against the
reference at the instruction level. `llvm-ml64` will assemble MASM, so the
reference file was assembled (after removing four textual `equ`s it does not
support, which is a smaller change than the translation itself) and both
objects disassembled and diffed. They agree instruction for instruction; the
only differences are alignment padding, and one instruction the *oracle* was
missing because of a flaw in its own preparation.

`tests/asm_parity.rs` then decodes every committed vector, every truncation
prefix of every vector, and 400 random single-bit corruptions of each, with
both loops, and requires identical bytes and identical errors. The full suite
passes with the `asm` feature, with `--no-default-features --features std`,
and as `no_std` + `alloc`, on aarch64 macOS, on x86_64 macOS under Rosetta and
on x86_64 Linux.

## The same loop on linux-x86_64

Commit `71d8ced`, the same harness, `--runs 3`, on the x86_64 bench box:
`12th Gen Intel(R) Core(TM) i5-1240P`, 16 logical CPUs, Ubuntu 26.04.1, gcc
15.2.0, `xz` 5.8.3. Everything below is pinned with `taskset -c 0-5` — ours
and every oracle alike — and the box was idle (load average under 1) before
each series. Fixtures were regenerated on the box, so their CRCs differ from
the macOS tables above; the payloads are generated by the same script to the
same parameters.

**Hybrid-core caveat.** This is an Alder Lake-P part: four P-cores with SMT
(CPUs 0-7, 4.4 GHz) followed by eight E-cores (CPUs 8-15, 3.3 GHz).
`taskset -c 0-5` pins every decoder, ours and the oracles alike, to the
threads of the first three P-cores, which is the only way a single-threaded
number on this machine means anything: an unpinned run can be migrated onto an
E-core mid-decode and lose a large fraction of its throughput for reasons that
have nothing to do with the code. It also means every figure here is a P-core
figure and says nothing about what the same binary does on an E-core. Because
the pinning is identical for all six decoders, the ratios are unaffected by
the choice; only the absolute MiB/s would move.

**No distro `7zz`.** The box has no 7-Zip package, so the oracle was built
from the reference tree (shallow clone of `ip7z/7zip` at `0766b733`) with
`make -f makefile.gcc`. That build has **no assembly loop**: there is no
`LzmaDecOpt.o` in its link line, so a stock `makefile.gcc` `7zz` on Linux is
the C decoder and is not the thing the gate is stated against. The gate
therefore uses a second binary, `bin-asm/7zz`, built from the same tree with
`-DZ7_LZMA_DEC_OPT` and `Asm/x86/LzmaDecOpt.asm` assembled in — 7-Zip's own
fast path, which is what `7zz` ships as on x86_64 elsewhere. `7lzma` is the
tree's `C/Util/Lzma` build, C only.

| fixture | ours (asm) | ours (portable) | `7lzma d` (C) | `7zz t -mmt=1` (asm) | `xz -T1` | asm vs 7zz |
| --- | --- | --- | --- | --- | --- | --- |
| `p256.bin.lzma` | 4.865 s | 5.844 s | 10.344 s | 4.897 s | 5.106 s | **0.994** |
| `payload.bin.lzma` | 19.514 s | 23.383 s | 41.177 s | 19.596 s | 20.458 s | **0.996** |
| `p256.bin.xz` (LZMA2) | 4.884 s | 5.860 s | n/a | 5.007 s | 5.127 s | **0.976** |

The gate is met on x86_64 as it is on aarch64: 0.976, 0.994 and 0.996, all
inside 3% and all on the fast side of it. The portable loop is at 1.19 of
`7zz`, the same order as its 1.36 on aarch64.

One number here is not like its macOS counterpart. `7lzma` — the same C source
the portable loop is a port of — runs at 24.9 MiB/s here against 45 MiB/s on
the Mac, so our portable loop is 0.57 of it rather than 0.90. `xz` is *faster*
here than on the Mac (50 vs 43 MiB/s), so this is not a slow machine; it is
gcc 15 compiling that inner loop about twice as badly as clang does. It is
worth recording because it makes "within 3% of 7-Zip" and "2x the reference C
decoder" the same sentence on this box, and only the first of those is a claim
about this crate.

## LZMA2 MT

The multi-threaded LZMA2 decoder (`Lzma2ParallelDecoder`, the port of
`C/Lzma2DecMt.c` over `C/MtDec.c`). Commits on `feature/lzma2-mt`: `3ecfa1f`
the MtDec/Lzma2DecMt port, `eedde6b` the run scanner, `a9efe4a` the adaptive
decoder, `5420779` and `161db14` the crypto trim and backend swap, `81c1de8` the
harness and `48a9a48` the fuzz target. The numbers below were taken with
the working tree at `161db14` plus the `std` gating fix that follows it,
which touches no decode path.

Fixtures: `mt.7z` and `st.7z` are the same 1 GiB payload, encoded by `7zz`
with `-mx5 -m0=lzma2` and `-mmt=on` / `-mmt=1`. Both are single-file,
single-folder archives, so the raw LZMA2 stream is one byte range; the
dictionary property byte (26, a 32 MiB dictionary, which `7zz l -slt` renders
as `LZMA2:25`) comes from the archive's own coder properties rather than from
that rendered string. `lzma-bench --index` prints what the encoder actually
produced:

| fixture | runs | unpacked per run |
| --- | --- | --- |
| `mt.7z` | 8 | 128 MiB |
| `st.7z` | 1 | 1 GiB |

**Eight runs is the ceiling.** A run is what is independently decodable, so
`mt.7z` cannot use more than eight threads no matter how many are asked for,
and both this crate and `7zz` stop scaling at eight. `st.7z` has one run, so
the parallel decoder has to notice that and decode it single-threaded,
streaming, without buffering the gigabyte.

### macOS, Apple M5 Max

`cargo run --release -p lzma-bench -- --runs 3 --threads sweep`, median of 3,
interleaved with the oracles. `7zz` 26.01. Load average 3.5 (a stray debug
test binary from an earlier session was pinned to a core throughout and could
not be killed from this session; it is one of eighteen, and it is charged to
every decoder equally by the interleaving).

`mt.7z` (8 runs), ours against `7zz t -mmt=N` and lzma-rust2 0.20.1's
`Lzma2ReaderMt` at the same thread count:

| threads | ours | MiB/s | peak RAM | `7zz t -mmt=N` | lzma-rust2 | vs 7zz | vs lzma-rust2 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 14.928 s | 68.6 | 33.0 MiB | 15.051 s | 24.252 s | **0.992** | **0.616** |
| 2 | 7.512 s | 136.3 | 481.3 MiB | 7.566 s | 24.234 s | **0.993** | **0.310** |
| 4 | 3.859 s | 265.4 | 962.1 MiB | 3.876 s | 24.220 s | **0.996** | **0.159** |
| 8 | 2.098 s | 488.0 | 1.9 GiB | 2.095 s | 24.220 s | **1.002** | **0.087** |
| 16 | 2.077 s | 493.1 | 1.9 GiB | 2.103 s | 24.629 s | **0.988** | **0.084** |
| 18 (all) | 2.087 s | 490.7 | 1.9 GiB | 2.095 s | 24.462 s | **0.996** | **0.085** |

lzma-rust2's peak was 610.9 MiB at every thread count, and its time did not
move with the thread count at all: it decodes this stream at one thread's
speed whatever it is asked for.

`st.7z` (1 run) — the no-overhead check. The parallel decoder must fall back
to the single-threaded path and pay nothing for having been asked for threads:

| threads | ours | peak RAM | `7zz t -mmt=N` | lzma-rust2 | vs 7zz | vs lzma-rust2 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 15.062 s | 33.0 MiB | 15.150 s | 24.665 s | **0.994** | 0.611 |
| 2 | 15.035 s | 144.3 MiB | 15.064 s | 24.329 s | **0.998** | 0.618 |
| 4 | 15.059 s | 144.3 MiB | 16.009 s | 25.357 s | **0.941** | 0.594 |
| 8 | 15.157 s | 144.3 MiB | 15.166 s | 24.566 s | **0.999** | 0.617 |
| 16 | 14.890 s | 144.3 MiB | 15.005 s | 24.144 s | **0.992** | 0.617 |
| 18 (all) | 14.965 s | 144.3 MiB | 15.090 s | 24.226 s | **0.992** | 0.618 |

144.3 MiB is what the threaded pass has allocated by the time the parse tells
it there is no second run: one thread's input chain and the 32 MiB dictionary
the single-threaded tail then runs with. lzma-rust2 buffers the whole run and
peaks at 2.8 GiB — a 19x difference on the case a chasing consumer meets most.

### The single-threaded gate, re-measured

Same session, same machine, `--runs 3`:

| fixture | ours | `7zz t -mmt=1` | `7lzma d` (C) | `xz -T1` | vs 7zz |
| --- | --- | --- | --- | --- | --- |
| `p256.bin.lzma` | 3.729 s | 3.728 s | 5.614 s | 5.799 s | **1.000** |
| `payload.bin.lzma` | 14.872 s | 14.886 s | 22.504 s | 23.095 s | **0.999** |
| `p256.bin.xz` (LZMA2) | 3.723 s | 3.805 s | n/a | 6.003 s | **0.978** |

The gate still holds, and on this run all three are at or on the fast side of
parity.

### Worker-side checksums, what they cost

`--runs 3 --threads 16 --checksum K` on `mt.7z`, one segment cut every
16 MiB, median of 3. The machine was the quietest it got in this session
(load average 4.1, a Docker VM and an unrelated e2e run owned the rest); the
`none` row is the control taken in the same series, not the 2.077 s from the
table above.

| checksum | ours | MiB/s | peak RAM | vs `none` |
| --- | --- | --- | --- | --- |
| none | 2.010 s | 509.4 | 1.9 GiB | — |
| CRC-32 | 1.997 s | 512.7 | 1.9 GiB | **0.994** |
| CRC-64/XZ | 2.051 s | 499.2 | 1.9 GiB | 1.020 |
| SHA-256 (AWS-LC) | 2.063 s | 496.4 | 1.9 GiB | 1.026 |

CRC-32 is free at this resolution; CRC-64/XZ costs 2% and SHA-256 2.6%, and
both numbers are what the per-byte rates below predict. Peak memory does not
move: a checksum is a few hundred bytes of state per worker.

Earlier, on a machine under load average 12, the same four runs came out
2.112 / 2.133 / 2.160 / 2.194 s — the same ordering and roughly the same
spreads, which is the useful check that these are real costs and not noise.

#### The per-byte rates underneath

`cargo run --release -p lzma-turbo --example checksum_cost --features
native-crypto`, a 256 MiB buffer, best of three passes, one core. The example
was deleted after the measurement; it is reproduced here because the numbers
are the answer to four separate questions.

```
crc-fast picked:
  CRC-32/ISO-HDLC  aarch64-neon-pmull-sha3
  CRC-64/XZ        aarch64-neon-pmull-sha3

one pass over the whole buffer:
  crc32             94205.7 MiB/s
  crc64_xz          69521.1 MiB/s
  sha256 (aws-lc)    3215.6 MiB/s
  sha256 (sha2)      3230.7 MiB/s
```

* `crc-fast` 1.10 selects `aarch64-neon-pmull-sha3` on its own, at runtime.
  Its optional `optimize_crc32_*` and `vpclmulqdq` cargo features are
  vestigial in 1.10 — nothing in its sources reads them — and its
  `feature_detection.rs` picks the tier from `OnceLock<ArchOpsInstance>`
  under `std`, which this crate already turns on. There is nothing to enable.
* Both SHA-256 backends run at ~3.2 GiB/s per core, well past the ~2 GB/s the
  ARMv8 SHA2 extensions were expected to give, so both are on them.
  RustCrypto's `sha2` 0.11 has no `asm` feature to turn on any more — it
  detects at runtime through `cpufeatures`, and matching AWS-LC to within 0.5%
  is the proof it found the instructions.

Cutting segments is free at any resolution this decoder uses:

```
cut into segments (crc32), to price the finalize/restart at each cut:
  268435456 B/segment (     1 segments)   92203.8 MiB/s
   67108864 B/segment (     4 segments)   93843.1 MiB/s
   16777216 B/segment (    16 segments)   94404.0 MiB/s
    1048576 B/segment (   256 segments)   89385.5 MiB/s
      65536 B/segment (  4096 segments)   55162.0 MiB/s
       4096 B/segment ( 65536 segments)   48425.6 MiB/s
```

A cut costs a finalize and a restart, nothing more, and that only starts to
show below about 1 MiB per segment. The 16 MiB stride the harness uses is
indistinguishable from one segment over the whole block. A consumer splitting
per 7z sub-stream — files, which are usually far bigger than a megabyte — pays
nothing for it.

Folding, by contrast, is not free, which is why the folder folds on query and
never eagerly:

```
      16 pieces  305.417µs  (19.09 us/piece)
     256 pieces  3.288708ms  (12.85 us/piece)
    4096 pieces  39.37825ms  (9.61 us/piece)
```

Each `crc_fast::checksum_combine` builds a GF(2) matrix, ~10-19 µs. That is
per *piece folded*, not per byte, and a consumer asking for one range per file
folds a handful of pieces — but it is why `CrcFolder` answers a range by
walking the pieces it needs rather than maintaining a running combination.

#### Which lever helped

None of them, and that is the finding: every one was already in the state the
operator asked for.

1. The checksum phase already runs on the worker thread, on its own still-warm
   output, immediately after the code loop and *before* `can_write.wait()` —
   see `mt/mtdec.rs`. No other worker's write can intervene, because the
   worker does not hold and is not waiting on a token while it checksums.
2. `crc-fast` is on the carry-less path with no feature work (above).
3. Segment cuts are free at this resolution (above).
4. Both SHA-256 backends are on the CPU's SHA extensions (above).
5. The only sync the feature adds is one `Mutex` push of a `BlockChecks` per
   output block, inside the write section that was already serialised — a
   pointer-sized move under an uncontended lock, next to a 128 MiB
   `write_all`. It cannot delay the next dispatch measurably, and the `none`
   and CRC-32 rows above are within noise of each other, which is the
   end-to-end version of the same statement.

### What mattered

Nothing in the decoder. The first complete measurement had the parallel path
at 1.10 of `7zz` at eight threads and above, while being at 1.02 at one and
two — an overhead that grew with the thread count, which is the signature of a
serial section rather than of slow decoding.

Instrumenting the ring's five phases (wait for the read token, read and parse,
decode, wait for the write token, write) settled it in one run. Summed over
eight threads of a 2.35 s decode: decode 15.74 s, wait-read 1.28 s, wait-write
1.03 s, **write 0.37 s**, read and parse 0.07 s. The write is the only
serialised section in the ring — one thread holds the write token at a time —
and 0.37 s of it against a 2.35 s wall is 16%, with the wait-write column
being the other threads queued behind it.

That 0.37 s was the **benchmark harness's own CRC32**, a slicing-by-8 table
implementation, running inside the sink. `7zz t` checksums its output too, but
with a fast one. Replacing the harness's CRC with the crate's `crc-fast` one
took the eight-thread figure from 2.336 s to 2.078 s and the ratio from 1.097
to 0.993, with no change to the decoder at all.

Two things follow, and the second is the reason this is written down.
Measuring a push decoder charges the sink to the decoder, so the sink has to
be as fast as the oracle's; and the expected culprits from the brief — output
copy on the writer thread, the parser starving workers, allocation per run,
channel wakeups per chunk — were all absent from the profile. Read and parse
was 0.4% of thread time. A tried change to allocate the ring's buffers with
`calloc` semantics instead of `try_reserve` + `resize` (saving a memset of
every input buffer and every output block) moved the eight-thread time by
nothing at all, 2.336 s to 2.336 s, and was reverted rather than kept: it
bought an `unsafe` block for no measurement.

### linux-x86_64

Same commits, same harness, `--runs 3`, on the x86_64 bench box:
`12th Gen Intel(R) Core(TM) i5-1240P`, 16 logical CPUs, Ubuntu 26.04.1. The
box was idle before the series (load average 0.5) and nothing was left running
after it. `7zz` is `bin-asm/7zz`, the reference tree built with
`-DZ7_LZMA_DEC_OPT` and `Asm/x86/LzmaDecOpt.asm` — see the note above about
why a stock `makefile.gcc` build is not the right oracle. Fixtures were
regenerated on the box, so their CRCs differ from the macOS tables; the run
layout is identical (8 runs of 128 MiB in `mt.7z`, 1 in `st.7z`).

This is an Alder Lake-P part: 4 P-cores with SMT on CPUs 0-7, 8 E-cores on
CPUs 8-15. Both lanes are recorded, because on this machine they say different
things.

`mt.7z`, **unpinned** (what an application gets):

| threads | ours | MiB/s | peak RAM | `7zz t -mmt=N` | lzma-rust2 | vs 7zz | vs lzma-rust2 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 20.397 s | 50.2 | 33.0 MiB | 20.511 s | 29.615 s | **0.994** | **0.689** |
| 2 | 10.816 s | 94.7 | 481.3 MiB | 10.753 s | 29.216 s | **1.006** | **0.370** |
| 4 | 6.535 s | 156.7 | 962.1 MiB | 6.515 s | 29.254 s | **1.003** | **0.223** |
| 8 | 4.257 s | 240.5 | 1.9 GiB | 3.921 s | 29.297 s | 1.086 | **0.145** |
| 16 (all) | 4.167 s | 245.7 | 1.9 GiB | 3.943 s | 29.040 s | 1.057 | **0.143** |

`mt.7z`, **pinned to the P-cores** (`taskset -c 0-7`, ours and every oracle
alike):

| threads | ours | MiB/s | `7zz t -mmt=N` | lzma-rust2 | vs 7zz | vs lzma-rust2 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 19.764 s | 51.8 | 20.085 s | 28.616 s | **0.984** | **0.691** |
| 2 | 10.385 s | 98.6 | 10.653 s | 28.605 s | **0.975** | **0.363** |
| 4 | 6.394 s | 160.2 | 6.388 s | 28.598 s | **1.001** | **0.224** |
| 8 | 4.332 s | 236.4 | 4.141 s | 28.596 s | **1.046** | **0.151** |

`st.7z`, the no-overhead check, unpinned and pinned:

| lane | threads | ours | peak RAM | `7zz t -mmt=N` | vs 7zz |
| --- | --- | --- | --- | --- | --- |
| unpinned | 1 | 19.739 s | 33.0 MiB | 20.034 s | **0.985** |
| unpinned | 4 | 19.798 s | 144.3 MiB | 20.072 s | **0.986** |
| unpinned | 16 | 19.817 s | 144.3 MiB | 20.041 s | **0.989** |
| `-c 0-7` | 1 | 19.758 s | 33.0 MiB | 20.024 s | **0.987** |
| `-c 0-7` | 8 | 19.812 s | 144.3 MiB | 19.996 s | **0.991** |

The single-threaded gate, re-measured in the same series with `taskset -c 0-5`
as the tables above it use:

| fixture | ours | `7zz t -mmt=1` | `7lzma d` (C) | `xz -T1` | vs 7zz |
| --- | --- | --- | --- | --- | --- |
| `p256.bin.lzma` | 4.856 s | 4.921 s | 10.347 s | 5.122 s | **0.987** |
| `payload.bin.lzma` | 19.551 s | 19.614 s | 41.183 s | 20.481 s | **0.997** |
| `p256.bin.xz` (LZMA2) | 4.870 s | 5.101 s | n/a | 5.148 s | **0.955** |

**The two rows that miss the gate, and why.** Unpinned at 8 and 16 threads
this crate is 8.6% and 5.7% behind `7zz`, while every other row on this box —
and every row on the Mac — is inside 5%, most of them on the fast side. The
pinned lane at 8 threads is 1.046, inside it.

`mt.7z` has exactly 8 runs, and the ring gives each worker one of them, so the
decode finishes when the *slowest* worker finishes. On a hybrid part a worker
that lands on an E-core takes about 1.7x as long as one on a P-core, and with
eight equal 128 MiB blocks and no way to shed work, that worker sets the wall
clock. Both decoders are exposed to this identically — it is the same
structure in both, and `7zz` also stops scaling past 8 — so what is being
measured in those two rows is which process the kernel's placement happened to
favour, over a window the size of one block. Pinning removes the asymmetry and
the difference goes with it. It is recorded rather than explained away because
it is what an application on this machine will see, and the honest statement
is: on a homogeneous machine this crate is at parity with 7-Zip across the
whole curve, and on a hybrid one the 8-thread point depends on core placement
to the tune of a few per cent in either direction.

Note the `7lzma` column, as in the single-threaded table above it: gcc 15
compiles the reference C loop about twice as badly here as clang does on the
Mac, so "within 3% of 7-Zip" and "2.1x the reference C decoder" are the same
sentence on this box. Only the first is a claim about this crate.

### Reading the runs in parallel: `decode_at`

On a 16-thread Sapphire Rapids (`c7i.4xlarge`, Ubuntu, rustc 1.97.1, 7-Zip
26.04's `7zz`), the 1 GiB `mt.7z` decoded at 0.90 of `7zz`'s speed, a gap the
256 MiB rows did not show. A per-block trace found the cause in the ring's one
serial stage. A worker reads its whole block while it holds the read token, so
block starts are staggered by the read time: about 110 ms per block here, which
put the last block's start 886 ms into a 3.9 s decode. Two things inflated the
read. The bench's stream path copied every byte twice through a 1 MiB
`BufReader`. And each fresh 256 KiB input link was zero-filled by hand before
it was read into.

Three changes, measured in one interleaved run. Each lane verified its CRC-32
first, then ran seven rounds; the medians are in seconds. `7zz` is
`7zz t -mmt=16`, timed as a whole process.

| `mt.7z`, 1 GiB | s | 7zz ÷ ours |
| --- | --- | --- |
| before | 4.175 | 0.906 |
| zeroed alloc, no copy | 4.039 | 0.936 |
| `decode_at` | 3.388 | 1.116 |
| `7zz` | 3.781 | |

| `p256.mt.7z` | s | 7zz ÷ ours |
| --- | --- | --- |
| before | 3.228 | 0.987 |
| zeroed alloc, no copy | 3.190 | 0.999 |
| `decode_at` | 3.102 | 1.027 |
| `7zz` | 3.187 | |

Allocating the links already zeroed is the change tried and reverted above,
when it bought nothing on an eight-thread Mac. The trace is what made it worth
trying again. It now reuses the crate's shared `zeroed_vec`, so it adds no
unsafe code, and at 16 threads it is worth 3% on the 1 GiB file.

`decode_at` removes the serial stage instead of shortening it. Each worker
finds its run from the chunk headers and reads it itself, so every block
starts at once. That takes the 1 GiB file from 10% behind `7zz` to 12% ahead
and the 256 MiB file to 3% ahead, at the same peak allocation (2.01 GB and
503 MB). It needs a source that can be read at any offset, which a `.7z` on
disk always is.

## xz container

Measured on linux-x86_64 (Arrow Lake-H, 16 threads, gcc 15, no AVX-512) with
`lzma-bench --xz`, three runs, median, on 2026-09-16. macOS aarch64 and
windows-msvc numbers follow in their own sections below.

Oracles are `xz -dc -T<n>` (5.8.3), `7zz t -mmt=1` (7-Zip 25.01) and the
`liblzma` crate 0.4.8 driving the same C library through
`MtStreamBuilder`/`XzDecoder`. Ratios are ours/theirs, so below 1 is faster.

### Sequential, one thread

| fixture | shape | `XzReader` | `xz -dc -T1` | ratio | `7zz t` | liblzma ST |
| --- | --- | --- | --- | --- | --- | --- |
| `p256.bin.xz` | 1 block | 4.996 s | 5.120 s | 0.976 | 10.387 s | 5.193 s |
| `p256.t8.xz` | 11 blocks | 4.935 s | 5.145 s | 0.959 | 10.376 s | 5.260 s |
| `p256.b16.xz` | 16 blocks | 4.920 s | 5.184 s | 0.949 | 10.465 s | 5.190 s |
| `payload.t8.xz` | 43 blocks, 1 GiB | 19.731 s | 20.601 s | 0.958 | 41.480 s | 20.903 s |
| `multi.xz` | several streams | 14.767 s | 15.419 s | 0.958 | 31.053 s | 15.323 s |
| `delta.xz` | delta + LZMA2 | 5.393 s | 5.620 s | 0.960 | 11.111 s | 5.625 s |
| `p256.sha256.xz` | SHA-256 check | 5.054 s | 5.746 s | 0.880 | 10.544 s | 5.744 s |
| `p256.crc32.xz` | CRC32 check | 4.987 s | 5.196 s | 0.960 | 10.503 s | 5.195 s |

The gate is "within 3% of `xz -dc -T1` and of `7zz t`". Every fixture above is
*faster* than both, so the gate passes with margin on all of them. The SHA-256
row is the widest: `xz` checks with its own C SHA-256 and this crate uses
AWS-LC's, which on this part is the difference between 44.6 and 50.6 MiB/s of
whole-file throughput.

`bcj-x86.xz` is not in the table. It is 99 KiB, decodes in 5 ms, and the ratio
it prints (1.83) is process startup and page faults, not decode: at that size
the oracle's own runs vary by more than the number being compared. The BCJ
filters are covered by the correctness tests and by the 256 MiB fixtures, and
a timing gate on a 5 ms workload would be measuring the shell.

### Parallel

`p256.t8.xz` (11 blocks), unpinned:

| threads | ours | MiB/s | peak RAM | `xz -T<n>` | liblzma MT | vs xz | vs liblzma |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 4.957 s | 51.6 | 66.2 MiB | 5.149 s | 5.102 s | 0.963 | 0.972 |
| 2 | 2.693 s | 95.0 | 132.3 MiB | 3.084 s | 3.105 s | 0.874 | 0.867 |
| 4 | 1.691 s | 151.4 | 267.5 MiB | 1.790 s | 1.766 s | 0.945 | 0.958 |
| 8 | 1.136 s | 225.4 | 480.8 MiB | 1.236 s | 1.220 s | 0.919 | 0.931 |
| 16 | 0.684 s | 374.2 | 480.9 MiB | 0.772 s | 0.712 s | 0.886 | 0.961 |

`p256.b16.xz` (16 blocks), unpinned:

| threads | ours | MiB/s | peak RAM | `xz -T<n>` | liblzma MT | vs xz | vs liblzma |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 5.007 s | 51.1 | 44.1 MiB | 5.179 s | 5.276 s | 0.967 | 0.949 |
| 2 | 2.569 s | 99.7 | 118.3 MiB | 3.007 s | 2.936 s | 0.854 | 0.875 |
| 4 | 1.551 s | 165.0 | 266.6 MiB | 1.633 s | 1.618 s | 0.950 | 0.959 |
| 8 | 0.880 s | 291.0 | 418.9 MiB | 0.944 s | 0.896 s | 0.932 | 0.982 |
| 16 | 0.595 s | 430.6 | 481.2 MiB | 0.687 s | 0.637 s | 0.865 | 0.933 |

`payload.t8.xz` at 8 threads: 3.543 s against 3.727 s (`xz -T8`, 0.951) and
3.649 s (liblzma MT, 0.971), 1 GiB out in 622 MiB of RAM.

Both gates pass: every parallel point is faster than `xz -dc -T8` at the same
thread count (the 8-thread rows are 8% and 7% ahead, well inside the 5%
allowance, which is an allowance to be *slower*), and every point at 1, 2, 4,
8 and 16 threads is faster than liblzma MT.

`multi.xz` at 4 threads is the interesting row: 6.144 s against 15.441 s for
`xz -T4` (0.398) and 5.106 s for liblzma MT (1.203). `xz` will not thread a
*concatenated* file at all on decode - it falls back to the sequential path,
which is why its number is the sequential number - so 0.398 is real but says
more about `xz` than about this crate. Against liblzma we are 20% behind
there, and the reason is visible in the RAM column: 1.4 GiB. Each stream's
index is read before its blocks can be scheduled, so at a stream boundary the
pipeline drains and refills, and the memory bound then admits a wide batch at
once. Streams are scheduled one at a time by design (the index of stream *n+1*
is not known until stream *n* is walked); a cross-stream scheduler is possible
and is not in this milestone.

Pinned to cores 0-7 (`taskset -c 0-7`), the same fixtures at 1/2/4/8: the
sequential ratios move to 0.951-0.961 and the parallel ratios to 0.853-0.964
against `xz` and 0.849-0.970 against liblzma. Pinning costs the 8-thread point
about 10% in absolute time (1.247 s vs 1.136 s on `p256.t8.xz`) because the
E-cores are excluded, and it changes no verdict.

Peak RAM is the decoder's own high-water mark. liblzma's column reads 8.2 KiB
because the C library writes straight into the caller's buffer and that buffer
is not counted; the comparison to make there is against `xz -T<n>`'s own
`--memlimit` behaviour, not against 8 KiB. Ours is bounded by the memory limit
in `XzOptions`, which is what the `memory_estimate` API reports.

### The adaptive decoder on a file already on disk

The `sevenz-turbo` fork measured `Lzma2AdaptiveDecoder` at 1.50x its own
parallel path on an archive on disk and asked for a decoder that stands aside
for its workers. `lzma-bench --adaptive` is that measurement: a 7-Zip archive
of 897 MiB packed, fed 16 MiB at a time and drained as it goes, timed against
`Lzma2ParallelDecoder` on the same stream. linux-x86_64, three runs, median.

| threads | chasing | waiting (`set_chase(false)`) | ring | waiting / ring |
| --- | --- | --- | --- | --- |
| 1 | 21.499 s | 21.773 s | 21.892 s | 0.995 |
| 2 | 21.405 s | 12.404 s | 12.145 s | 1.021 |
| 4 | 22.032 s | 7.026 s | 7.064 s | 0.995 |
| 8 | 20.333 s | 6.625 s | 6.848 s | 0.967 |

Pinned to cores 0-7 the same shape holds: 20.3 s / 20.3 s / 20.1 s at one
thread, and 6.604 s against the ring's 6.794 s at eight.

The "chasing" column is the default, and on this workload it is a flat line:
the chase decoder reaches every run before the feed that would complete it, so
the whole archive decodes on the calling thread however many threads are
available. Three changes were needed to get the other column:

1. the chase stands aside while a worker is outstanding;
2. `set_chase(false)` lets a caller whose bytes are already on disk say that an
   incomplete run means "not fed yet", not "not written yet";
3. `drain` hands control back instead of blocking on a worker while input is
   still coming - without this the decoder dispatches one run, waits for it,
   and only then reads the next, which is a serial decode with extra steps.

With all three, the adaptive decoder is within 3% of the ring at every thread
count, which is the point: the fork can drop its gigabyte of read-ahead and
use the driver whose input it can borrow.

### macOS aarch64

Measured on this Mac (Apple M5 Max, 18 threads) on 2026-09-16, same
`lzma-bench`, three runs, median. The 1-minute load average was 2.85 when the
series started; the machine owner's own applications, not this crate, are the
background. `xz` is 5.8.3 and `7zz` is 7-Zip 25.01.

| fixture | shape | `XzReader` | `xz -dc -T1` | ratio | `7zz t` | liblzma ST |
| --- | --- | --- | --- | --- | --- | --- |
| `p256.bin.xz` | 1 block | 3.639 s | 5.880 s | 0.619 | 3.745 s | 5.791 s |
| `p256.sha256.xz` | SHA-256 check | 3.832 s | 6.306 s | 0.608 | 3.847 s | 6.244 s |
| `delta.xz` | delta + LZMA2 | 4.500 s | 6.875 s | 0.655 | 4.826 s | 6.831 s |
| `p256.t8.xz` | 22 blocks | 4.152 s | 6.375 s | 0.651 | 4.106 s | 6.238 s |
| `p256.b16.xz` | 16 blocks | 3.804 s | 5.997 s | 0.634 | 3.768 s | 5.918 s |

The sequential gate is against `7zz t -mmt=1`, and that is the column to read:
0.97-1.03 on every fixture, so the crate is level with 7-Zip's own assembly
loop on aarch64 and the 0.61-0.66 against `xz` is the C library's LZMA2 being
slower than 7-Zip's, not a margin this crate created.

`p256.t8.xz`, parallel:

| threads | ours | MiB/s | peak RAM | `xz -T<n>` | liblzma MT | vs xz | vs liblzma |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 3.870 s | 66.2 | 33.3 MiB | 6.174 s | 6.080 s | 0.627 | 0.636 |
| 2 | 2.011 s | 127.3 | 89.1 MiB | 3.182 s | 3.163 s | 0.632 | 0.636 |
| 4 | 1.115 s | 229.6 | 190.2 MiB | 1.785 s | 1.794 s | 0.624 | 0.621 |
| 8 | 0.569 s | 449.8 | 369.8 MiB | 0.917 s | 0.966 s | 0.621 | 0.589 |
| 16 | 0.433 s | 590.9 | 482.8 MiB | 0.651 s | 0.667 s | 0.665 | 0.649 |
| 18 | 0.418 s | 611.8 | 482.9 MiB | 0.676 s | 0.663 s | 0.619 | 0.631 |

`p256.b16.xz`, parallel:

| threads | ours | MiB/s | peak RAM | `xz -T<n>` | liblzma MT | vs xz | vs liblzma |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 3.687 s | 69.4 | 44.3 MiB | 5.852 s | 5.798 s | 0.630 | 0.636 |
| 2 | 1.924 s | 133.1 | 118.8 MiB | 3.050 s | 3.032 s | 0.631 | 0.634 |
| 4 | 0.998 s | 256.6 | 267.7 MiB | 1.575 s | 1.554 s | 0.633 | 0.642 |
| 8 | 0.520 s | 491.9 | 388.3 MiB | 0.819 s | 0.793 s | 0.635 | 0.656 |
| 16 | 0.335 s | 765.2 | 482.7 MiB | 0.488 s | 0.452 s | 0.685 | 0.741 |
| 18 | 0.295 s | 866.5 | 482.7 MiB | 0.551 s | 0.452 s | 0.537 | 0.654 |

866 MiB/s of output at 18 threads, and both gates pass with room: every
parallel point is faster than `xz -T<n>` and than liblzma MT at every thread
count measured.

The adaptive decoder on `mt.7z` (897 MiB packed, 8 runs of 128 MiB), against
the ring:

| threads | chasing | waiting (`set_chase(false)`) | ring | waiting / ring |
| --- | --- | --- | --- | --- |
| 1 | 14.952 s | 14.842 s | 15.190 s | 0.977 |
| 2 | 14.890 s | 7.659 s | 7.577 s | 1.011 |
| 4 | 14.903 s | 4.011 s | 3.875 s | 1.035 |
| 8 | 15.349 s | 2.134 s | 2.075 s | 1.028 |

Same shape as on linux-x86_64: chasing a file that is already on disk pins the
decode to one thread, and `set_chase(false)` puts it within 3.5% of the ring
at every thread count. The 1-minute load had climbed to 7.8 by the end of the
adaptive series (the machine owner's applications), which is the most likely reason
the 4- and 8-thread ratios sit a little above the 0.97-1.02 measured on the
quiet Linux box; the verdict is the same either way.

### windows-msvc

Measured on windows-x86_64 (Ryzen 5 3600, 6C/12T, Windows) on 2026-09-16, same
`lzma-bench --xz`, three runs, median. The build is `clang-cl` with AWS-LC
linked statically; the CPU reports itself as "AMD Ryzen 5 3600 6-Core
Processor", the toolchain is rustc 1.97.1 (8bab26f4f), and `xz` is the 5.8.1
that ships with Git for Windows (so the oracle there is a version behind the
Linux box's 5.8.3). The box was checked
idle first, and nothing else of anyone's was running.

| fixture | shape | `XzReader` | `xz -dc -T1` | ratio | liblzma ST |
| --- | --- | --- | --- | --- | --- |
| `p256.bin.xz` | 1 block | 5.028 s | 5.481 s | 0.917 | 5.706 s |
| `p256.sha256.xz` | SHA-256 check | 5.140 s | 6.207 s | 0.828 | 6.264 s |
| `delta.xz` | delta + LZMA2 | 5.483 s | 6.065 s | 0.904 | 6.296 s |
| `p256.t8.xz` | 22 blocks | 5.058 s | 5.491 s | 0.921 | 5.711 s |

`p256.t8.xz`, parallel:

| threads | ours | MiB/s | peak RAM | `xz -T<n>` | liblzma MT | vs xz | vs liblzma |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 5.241 s | 48.8 | 33.3 MiB | 5.691 s | 5.951 s | 0.921 | 0.881 |
| 2 | 2.702 s | 94.7 | 66.5 MiB | 2.864 s | 3.064 s | 0.943 | 0.882 |
| 4 | 1.543 s | 165.9 | 211.4 MiB | 1.609 s | 1.718 s | 0.959 | 0.898 |
| 8 | 0.851 s | 300.8 | 326.0 MiB | 0.922 s | 0.968 s | 0.923 | 0.879 |
| 12 | 0.668 s | 383.0 | 405.9 MiB | 0.732 s | 0.767 s | 0.913 | 0.872 |
| 16 | 0.721 s | 355.3 | 482.8 MiB | 0.767 s | 0.829 s | 0.939 | 0.869 |

`p256.b16.xz` (16 blocks of 16 MiB), parallel:

| threads | ours | MiB/s | peak RAM | `xz -T<n>` | liblzma MT | vs xz | vs liblzma |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 5.068 s | 50.5 | 44.3 MiB | 5.464 s | 5.710 s | 0.928 | 0.888 |
| 2 | 2.615 s | 97.9 | 118.8 MiB | 2.789 s | 2.956 s | 0.937 | 0.884 |
| 4 | 1.379 s | 185.7 | 251.6 MiB | 1.479 s | 1.534 s | 0.932 | 0.899 |
| 8 | 0.812 s | 315.2 | 420.3 MiB | 0.881 s | 0.914 s | 0.922 | 0.889 |
| 12 | 0.797 s | 321.3 | 420.4 MiB | 0.869 s | 0.913 s | 0.917 | 0.873 |
| 16 | 0.660 s | 387.7 | 482.7 MiB | 0.757 s | 0.757 s | 0.872 | 0.873 |

`p256.b16.xz` sequential: 5.058 s against 5.477 s (0.924) and 5.725 s for
liblzma ST.

Both gates pass on Windows as they do on Linux: every sequential fixture is
ahead of `xz -dc -T1`, and every parallel point is ahead of both `xz -T<n>`
and liblzma MT: the widest spread is 0.959 and the narrowest margin is still
4%. On this 6-core part the 12- and 16-thread points are within noise of each
other - both oracles behave the same way there - so the scaling story ends at
about 8 real cores, as it should.

The same toolchain was used to prove the AWS-LC build twice: once with its
assembly generated from source by NASM (`AWS_LC_SYS_PREBUILT_NASM=0`, with
`nasm.exe` visible in `cargo build -vv`) and once through the prebuilt
objects. Both link and both decode the fixtures to the same CRC-32; the PE
binaries themselves hash differently, which is expected of two different
object inputs and is why the decoded content, not the binary, is the thing
compared.
