# arm64 LZMA decode-loop tuning on Apple M5 Max

Written before the decoder's arm64 loop was ported. The port took this advice:
`src/lzma/decode_opt/lzma_dec_opt_aarch64.S` is the SDK's stock loop, built
into a Rust naked function by `src/lzma/decode_opt/aarch64.rs`.
Full experiment log with every number: `asm-lab/RESULTS.md`.

## TL;DR

I tried to beat 7-Zip's hand-written `Asm/arm64/LzmaDecOpt.S` on this M5 Max and
**could not**. Five structural rewrites were built, verified byte-exact and
measured; four were slower and one tied on a short stream but lost on the real
fixture. The final `asm-lab/LzmaDecOpt-m5.S` is therefore **code-identical to
stock** — `diff` against `asm-lab/LzmaDecOpt-stock.S` shows only a comment
block.

**What this means for you: port the stock loop faithfully. Do not "improve" the
bit macros on the way through.** The four rewrites below all look like clear
wins on paper. They are the obvious things a competent person does to this loop,
and every one of them lost on this core. If you find yourself reaching for one,
read the matching section first.

The asm loop is **1.54x faster than 7-Zip's own optimised C loop**
(3.80 s vs 5.85 s on the 256 MiB fixture), so the asm is very much worth having;
it is just already at its local optimum.

## The performance model you need to design against

Measured on the project fixtures: **~59.9 cycles per output byte**, ~9 binary
decodes per byte, so **~6.65 cycles per binary decode**. `objdump` shows the
unrolled literal tree at **20 instructions per bit** (17 issue in the common
case; the 3-instruction refill body is branched over ~7 times in 8).

Two limits sit almost exactly on top of each other:

1. **Loop-carried latency ~6.30 cycles.** The chain is
   `csel(range) -> lsr #11 -> mul -> subs`. Measured in isolation with
   `asm-lab/chainbench.c` at 6.30 cyc/iter (clock 4.21–4.24 GHz).
2. **Issue throughput.** Inserting *N* genuinely independent, dead-result
   instructions into `BIT_1_R` costs **~0.32 cycles each** — +1 instruction
   measured at **+3.7%**.

6.30 against 6.65 achieved leaves ~0.35 cycles of slack. Consequences:

* **Adding an instruction costs ~0.32 cycles immediately.** Nothing absorbs it.
* **Removing an instruction buys nothing**, because the 6.30-cycle recurrence
  then becomes the binding constraint.
* A win needs to shorten the recurrence *and* not grow the instruction count.
  Every recurrence-shortening formulation I found needs **more** instructions
  to express (see below), and the one instruction-removing formulation needs a
  memory access width that creates a store-forwarding hazard.

In Rust terms: whatever you do inside `asm!`, **count the instructions in the
generated literal-tree block** (`objdump` the built rlib) and treat 20/bit as
the budget. A `asm!` block that is one instruction fatter than stock will be
~4% slower, and that is bigger than any algorithmic tidiness is worth.

## ARM64 addressing constraints that close the obvious doors

The tree walk wants `child = probs[2*sym + bit]` and the update stores at
`probs[sym]`. Scaled-register addressing allows a shift of **0 or
log2(access size) only**:

| access | legal index shifts |
|---|---|
| `ldrh`/`strh` (16-bit `CLzmaProb`) | 0, 1 |
| `ldr w`/`str w` (32-bit `CLzmaProb`) | 0, 2 |
| `ldr x` (64-bit, whole child pair) | 0, 3 |

* 16-bit probs + undoubled `sym` would need `lsl #2` on an `ldrh` — **illegal**.
  That is exactly why stock carries the `add sym,sym`; it is not redundant.
* 32-bit probs + undoubled `sym` would need `lsl #3` on an `ldr w` — **illegal**.
  This killed a variant at the assembler. The only way to exploit 32-bit probs
  is a 64-bit pair load.
* `ldp` has **no register-indexed addressing form at all**, which kills any
  "load four grandchildren, decode two tree levels per addressing op" scheme.

## The four things that look right and are wrong

### 1. Do not make renormalisation branchless (-9.1%, the worst result)

Replacing `tst range,0xFF000000 / b.ne` + the branched-over 3-instruction
refill with 7 unconditional instructions (`lsl`/`csel` range, speculative
`ldrb [buf]`, `orr`/`csel` code, `cinc buf`) is **9.1% slower**.

**The renormalisation branch predicts well.** It fires roughly 1 bit in 8 and is
evidently far from random to the predictor. Paying 5 extra instructions on every
bit to remove that mispredict exposure is a bad trade by a wide margin. Keep the
branch.

(It would also have added an invariant you would have to honour: a speculative
`ldrb [buf]` on every bit reads up to **one byte past `bufLimit`**.)

### 2. Do not shorten the range recurrence with `msub` (-2.2%)

Carrying `rangeA = range >> 11` across iterations takes the `lsr` off the
critical path, and `mul` + `msub` computes `bound` and `range - bound` together
(the `msub` **addend→result latency is 1 cycle** on M5 — verified). Isolated
recurrence drops from **6.30 to 5.53 cycles**.

It still loses, because selecting the new `rangeA` needs an `lsr` from *both*
candidates plus a second `csel`, i.e. **+2 instructions per bit** where stock
needs one `csel` + one `lsr`. This is the most informative negative result of
the session: **a 0.77-cycle recurrence saving did not materialise in situ**, so
the loop is issue-limited rather than purely latency-limited, and the entire
"shorten the recurrence" family is retired.

### 3. Do not switch `CLzmaProb` to 32 bits (`Z7_LZMA_PROB32`) (no consistent effect)

32-bit probs put both children of node `sym` in one naturally-aligned 8-byte
pair at `8*sym`, so `ldr x8,[probs,sym,lsl#3]` fetches both: the low child is
the free `w8` view, the high child is one `lsr`. Keeping `sym` undoubled then
also collapses `add sym,sym` + `adc sym,sym,wzr` into one `adc sym,sym,sym` and
makes the parent store a plain `str [probs,sym,lsl#2]`. This genuinely removes
an instruction — **objdump-confirmed 19 per literal bit vs stock's 20**.

Measured four times, **with no consistent sign**: +0.5% on a 32 MiB slice,
**+4.5% (slower) on the 256 MiB fixture**, **−1.7% (faster) on the 1 GiB
fixture**. That is not a win, it is a change whose effect is below the
fixture-to-fixture variation. Meanwhile `Z7_LZMA_PROB32` *without* the pair load
is neutral (3.8063 vs 3.8025 s), which exonerates the doubled table and points
at the 8-byte load: the 4-byte prob store partially overlaps the later 8-byte
pair load of the same pair — textbook partial store-to-load forwarding. Dodging
it by storing a full 8-byte pair needs 4 extra instructions to splice the
updated half in.

**So: keep `CLzmaProb` as `u16` in the Rust port.** You get no speed for
doubling the prob table, and the pair-load form that would justify it is slower.

### 4. The copy-loop build flags do not matter

`LZMA_USE_2BYTES_COPY`, `LZMA_USE_CMOV_LZ_WRAP` and turning off
`LZMA_USE_4BYTES_FILL` were all measured on a deliberately match-heavy stream
(7-Zip source tar, ratio 0.26). All inside noise; enabling both copy options
together was marginally worse. Stock's defaults (`4BYTES_FILL` on, the other two
off) are fine. Match copy is **not** where the time goes.

## Things that were already optimal in stock — don't "fix" them

* **Alignment.** `MY_ALIGN_FOR_ENTRY = MY_ALIGN_FOR_LOOP = MY_ALIGN_32` and
  `lit_start` is `MY_ALIGN_64`. Verified in `objdump -d`. The `.p2align 5` item
  is already satisfied.
* **Literal unrolling.** `_LZMA_SIZE_OPT` is off, so the literal tree is fully
  unrolled into 8 `BIT_*` copies. The `#ifdef _LZMA_SIZE_OPT` rolled loop in the
  source is the *slow* path and is not built — don't port that one by mistake.
* **Matched-literal (LITM).** Already fully masked/branchless
  (`and bit,match,offs` + `cmovae offs,bit`). Nothing to convert.
* **Register residency.** No spills in the hot loop. It uses **x0–x17 and
  x19–x30**, with **x18 reserved on Apple platforms**. There are effectively
  zero spare registers: a variant that needed two more live values had to spill
  `bufLimit` (x16) and `checkDicSize` (x27) to the stack.
* **The probability update.** `kBitModelOffset = 2048-32+1 = 2017` fuses both
  update rules into `sub` + `csel` + `sub ..., asr #5`. Do not try to shorten it
  to two operations: the tempting identity `(p-2017)>>5 == (p>>5)-63` is
  **false exactly when `p % 32 == 0`**, and every correct alternative also needs
  three operations.

## Invariants and layout assumptions

The tuned loop is code-identical to stock, so it adds **no new invariants**.
The ones stock already relies on, which your `asm!` port must reproduce exactly:

**`CLzmaDec` struct offsets** (hardcoded in the asm; if your Rust struct differs
by one byte the loop silently corrupts):

| field | offset |
|---|---|
| `lc` / `lp` / `pb` | 0x00 / 0x01 / 0x02 |
| `probs` | 0x08 |
| `dic` | 0x18 |
| `dicBufSize` | 0x20 |
| `dicPos` | 0x28 |
| `range` / `code` | 0x38 (loaded as one `ldp` pair — `code` at 0x3c) |
| `processedPos` | 0x40 |
| `checkDicSize` | 0x44 |
| `reps[0..3]` | 0x48, 0x4c, 0x50, 0x54 (two `ldp` pairs) |
| `state` | 0x58 |
| total | 96 |

`range` and `code` are loaded with a single `LOAD_LZMA_PAIR`, and `reps` with
two — so those fields must stay adjacent and naturally aligned.

**Prob-table layout** (offsets in *entries*, scaled by `PMULT = 1 << PSHIFT`):
`SpecPos = 0`, `IsRep0Long = 128`, `RepLenCoder = 384`, `LenCoder = 896`,
`IsMatch = 1408`, `kAlign = 1664`, `IsRep = 1680`, `IsRepG0/1/2 = 1692/1704/1716`,
`PosSlot = 1728`, `Literal = 1984` (`= NUM_BASE_PROBS`).

**Register map** (`x18` is reserved; every other callee-saved register is in
use):

| reg | role |
|---|---|
| w0 | `range` |
| w1 | `prob` |
| w2 | `probBranch` / `cnt` / `prm` (LITM) |
| w3 | `sym` / `dist` |
| w4 | `t3` |
| w5 | `cod` |
| x6 | `t1` / `probs_state` |
| w7 | `t0` / `prob2` |
| w8 | `t2` |
| w9 | `t5` / `match` (LITM) / `sym2` (ShortDist) |
| x10 | `t4` / `offs` (LITM) / `probs_PMULT` (tree) / `numBits` |
| x11 | `probs` |
| w12 | `len` |
| w13 | `state` |
| x14 | `dicPos` |
| x15 | `buf` |
| x16 | `bufLimit` |
| x17 | `dicBufSize` |
| x19 | `limit` |
| w20–w23 | `rep0`–`rep3` |
| x24 | `dic` |
| x25 | `probs_IsMatch` |
| x26 | `probs_Spec` |
| w27 | `checkDicSize` |
| w28 | `processedPos` |
| w29 | `pbMask` |
| w30 | `lc2_lpMask` |

**`lc2_lpMask` does double duty**: it holds `(512 << (lc+lp)) + lc - 511` and is
used *both* as the shift amount (its low bits are `lc+1`) *and* as the mask. The
junk low bits are always below the shift, which is what keeps
`probs + Literal*PMULT + 3*sym` 4-byte aligned. If you recompute this value in
Rust, reproduce it exactly.

**Loop-carried state must be seeded at every entry into the bit macros.**
`len8_loop` jumps straight into `BIT_1` *without* a preceding `BIT_0`. A variant
that added one carried register produced `Error: Data error` only on streams
containing a long match, because of this one path. If your port carries any
extra state, audit every `jmp` into a `BIT_*` sequence.

## Measurement notes (read before trusting any number you take here)

* **Hardware CPU counters are unavailable on this box.** `xctrace` requires a
  full Xcode install (only Command Line Tools are present) and `instruments` is
  not installed. **There are no measured IPC or branch-mispredict counters in
  this work.** Every microarchitectural claim above is inferred from CPU time,
  from `chainbench.c` latency microbenchmarks, and from in-situ "add N dead
  instructions and measure" probes. The claims are well-replicated but they are
  inferences.
* The machine is shared. The lab's timing helper (since removed) reported **min-of-N child CPU time**,
  not median wall time, specifically because other builds on the box were
  saturating all 18 cores for much of this session (load average peaked at 24).
  Quiet-box repeatability is ~±0.5%; under load ~±3%. Check `uptime` before
  believing a small delta.
* `cargo xtask asm-lab verify` is the correctness gate: both project fixtures plus small
  streams from `xz --format=lzma` across presets **and lc/lp/pb combinations**.
  The lc/lp/pb spread matters — `lc2_lpMask` and `pbMask` are only exercised
  off their defaults by non-default settings, so a loop can be wrong and still
  pass on preset-6 data.

## Final numbers vs the shipped `7zz`

Apple M5 Max, macOS, 18 cores. `7zz` 26.01 (arm64, ASM) from Homebrew. All
figures are **min-of-N child CPU seconds**; both sides write no output
(`7lzma d <in> /dev/null` vs `7zz t -mmt=1`). Load average was ~7 (another
agent building), so treat sub-1% differences as noise.

### `p256.bin.lzma` — 256 MiB out, ratio 0.92 (literal-dominated)

| decoder | s (cpu) | MiB/s | ratio vs 7zz |
|---|---|---|---|
| `7zz t -mmt=1` | 3.8343 | 66.8 | 1.000 |
| **m5 asm loop (this work)** | **3.8630** | **66.3** | **1.007** |
| 7-Zip C loop (`LzmaDec.c`, no asm) | 5.6797 | 45.1 | 1.481 |

### `payload.bin.lzma` — 1024 MiB out, ratio 0.875 (also literal-dominated)

| decoder | s (cpu) | MiB/s | ratio vs 7zz |
|---|---|---|---|
| `7zz t -mmt=1` | 14.8651 | 68.9 | 1.000 |
| **m5 asm loop (this work)** | **14.9701** | **68.4** | **1.007** |

**Ratio vs `7zz`: 1.007 on `p256.bin.lzma` and 1.007 on `payload.bin.lzma`** —
the same figure on both fixtures. That is parity within noise, which is
the expected result: `7zz` runs the *same* asm, so the residual is CLI and I/O
overhead, not decode speed. The target of "below 1.0" was **not** reached, and
the reason is documented above: there was no win to be had in this loop on this
core.

The useful comparison is the last row: **the asm loop is 1.47x faster than
7-Zip's own optimised C**, which is the bar a Rust port has to clear.
