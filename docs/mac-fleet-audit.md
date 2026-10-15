# Mac fleet optimization audit

Final local checkpoint: 2026-10-15. The performance arc is stopped at the
operator's requested scope, with measured improvements retained and the
bounded XZ output-buffer and tiny non-solid CPU tradeoffs explicitly accepted.
The dated experiments below retain failed and superseded results; their
pending-status statements describe those cohorts, not the final checkpoint.

Local validation passes: both native workspace sweeps, all-features builds,
portable/no-default feature sweeps, doctests, workspace Clippy, formatting,
required pinned-SDK/XZ differential tests, all seven fuzz-target builds,
three targeted Miri ownership checks, and six explicit large/sparse tests.
Full cross-platform CI, the full sharded Miri selection and scheduled fuzz
mutation runs remain CI gates; this local pass does not claim their results.
See `mac-fleet-final-validation.json.zst` for commands, output, source hashes,
original Nextest pipe warnings and clean targeted rechecks.

The final normal-product decoder controls preserve timing: non-solid full
thread direct decode is 4.64% faster than the frozen product with CPU +1.00%;
streaming is 0.43% faster with CPU -0.52%. Direct RSS rises 753,664 bytes
(+4.75%), bounded at the same requested thread count, within the existing
large-wall-win RSS tradeoff for this workload. Streaming RSS falls 16,384
bytes. Every run delivered all 8,192 members and 268,910,327 bytes with
verification enabled. This control does not assert universal 3% RSS parity.

The reference is 7-Zip 26.03 (tag commit
`0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17`) and xz 5.8.3 (tag commit
`4b73f2ec19a99ef465282fbce633e8deb33691b3`). Measurements use the existing
fleet corpus, three interleaved repetitions, one warmup, and separate
correctness verification. Wall time, process CPU time, peak RSS and archive
size are distinct gates. Mac measurements do not establish other-platform
results.

## Earlier closure inventory (retained history)

Ordinary L5, encrypted L5 and zeros L5 T16 now pass on the three remote
hosts tested with the shared-cursor build. Current-source Mac timing remains
unverified. Work has resumed on the remaining intermediate-width residency
and decoder gaps. Core-isolated profiles are diagnostic; they do not replace
ordinary-scheduler wall, CPU or RSS acceptance measurements.

The eight tracking groups below distinguish measured gaps, fixes awaiting
fleet verification, proposed tradeoffs and marginal final checks. Passing
rows retain their closure; unmeasured fleet rows remain unverified.

| Group | Remaining scope |
| --- | --- |
| Full-thread ordinary encode timing | Current shared-cursor build passes ordinary and zeros T16 wall, CPU and memory limits on the tested Linux x86-64, Windows x86-64 and Graviton4 hosts. Complete cohorts, preceding-product comparisons and conflicting Windows charged counters remain below. Retained Mac T16 wall +3.66% / CPU +3.27%; current-source Mac timing and other fleet hosts remain unverified. |
| Full-thread encrypted encode timing | Current shared-cursor build passes AES T16 wall, CPU and memory limits on the tested Linux x86-64, Windows x86-64 and Graviton4 hosts. Earlier failures remain below. Retained Mac wall +3.43% / CPU +3.48%; current-source Mac timing and other fleet hosts remain unverified. AWS-LC is already using its bulk hardware path. |
| Intermediate-width and fast-finder encode timing/RSS | Completed-output retention closes Linux x86 L5 T4: wall +0.54%, CPU +0.70%, RSS +0.24%. All six broader encoder cases pass wall/CPU/RSS against SDK and stay within 3% of the preceding product, including L5 T8/T16/AES T16 and zeros T4. Current L3 T2 RSS is -9.00%; its historical +7.88% tradeoff remains operator-accepted. Fast-finder zeros RSS tradeoffs are also accepted below. Retained Mac T8 RSS +5.20% remains unverified on current source. |
| Non-solid T1 encode RSS | Operator accepted the reported tiny encrypted non-solid cases: x86 RSS +15.23%, wall -17.74% / CPU -18.15%; retained Mac RSS +6.57%. This accepts those measured tradeoffs, not arbitrary future regressions. Ordinary tree T1 passes on both hosts; unencrypted tiny files passed two x86 compact-folder cohorts. |
| Non-solid T1 decode RSS | Closed with operator-accepted scoped RSS exceptions on Linux x86: direct 8.125 versus 7.705 MiB (+0.420 MiB, +5.45%); streaming 8.637 versus 7.773 MiB (+0.863 MiB, +11.11%). Wall/CPU remain within 0.2% of SDK. Compact coder records and all verification are retained; public API unchanged. This accepts the measured overhead, not leaks or future regressions. Current-source Mac remains unverified. |
| x86 XZ filter RSS | Operator accepted the scoped whole-process RSS tradeoff after the complete allocation/mapping audit: about 13 MiB versus xz's 10.2 MiB (26–27% higher), with all ten pinned timing comparisons passing. Caller-output filtering removes private filter storage; unchanged caller buffers and executable mappings account for the main remaining excess. Symbol companion failed code/data identity and is not used for attribution. This is an accepted exception, not measured RSS parity or permission for future regressions. |
| BCJ2 marginal final-verification cells | Operator accepted the reported single-folder T16 CPU +3.16% and multi-folder T2 wall +3.10% cells. T2 pairing, streaming T2, T4 and T8 already pass. No further BCJ2 optimization is required for these measured cells; other-platform coverage remains separate. The complete cursor port remains diagnostic. |
| Near-incompressible encrypted decode | Latest ordinary-build x86 AES mx1 T16 wall +0.37%, CPU -1.91%, RSS -8.84%. The preceding product also passes this cohort, so this is not a new causal improvement. Current-source Mac and other fleet hosts remain unverified. |

For closure, identify the workload, input/delivery mode, thread pairing,
architecture, metric, product source and reference binary. A passing row
does not close another width or mode. Product-baseline improvements are
recorded separately from reference parity. Historical measurements below
remain evidence for their exact builds; they are not repeated closure of
the current build. A marginal row that changes sides of the 3% limit across
cohorts stays open pending confirmation.

### Operator decisions and active work, 2026-10-14

Accepted in response to the complete gap inventory: L3 T2 encoder RSS
+7.88%; tiny encrypted non-solid encoder RSS (reported x86 +15.23% and
Mac +6.57%); the two marginal BCJ2 cells; and fast-finder zeros L1 T2 /
L3 T16 RSS +8.23% / +14.51%. These are scoped measured exceptions, not
claims that the metrics are within 3%. Historical descriptions below remain
the record of decisions at their time and are superseded by these acceptances.

After the full decoder allocation and executable-mapping audit, the operator
also accepted the ten measured x86 XZ filter process-RSS cells, approximately
26–27% above xz with passing wall/CPU. The caller-output port is retained;
caller buffer sizes, features and benchmark behavior are unchanged.

The operator also accepted the recorded XZ encoder RSS tradeoffs, conditional
on bounded memory use rather than unbounded growth: single-thread presets
1/5/9 add 7.28/23.75/99.66 MiB over xz; branch-filter cases add approximately
6 MiB, and delta4/delta64 approximately 6.8/7.5 MiB. The associated measured
wall/CPU gains remain part of this acceptance. This does not accept leaks,
accumulation across encoder reuse, arbitrary future regressions, or establish
current-build measurements. Allocation/lifetime bounds and any format-required
metadata growth remain to be verified explicitly; the fixed-corpus timings
alone do not establish boundedness.

L5 T4 encoder RSS is closed on Linux x86 with the broader regression cohort
passing. Non-solid T1 decoder RSS is closed by the scoped operator acceptance
above, with timing passing and compatible allocation reductions retained.
The Windows borrowed-delivery candidate passes all seven wall, scheduled-CPU
and working-set comparisons against SDK and the preceding product, with all
147 traces accounted for. Remaining Mac/fleet measurements, earlier
product-regression reconciliation, XZ encoder memory-boundedness verification,
Weaver broad integration checks and the final Miri/format/lint capstone
remain tracked. Accepted ring and parallel-extraction exceptions remain intact.

Separate from these groups: the quiet LZMA/XZ encoder matrix completed all
17 scenarios, with conditionally accepted RSS tradeoffs recorded below. The other three
fleet architectures still need current-source verification. BCJ2's literal
T1/T1 pipeline discrepancy is a known comparison
contract, reconfirmed by the operator: compare our T2 with SDK T1 or use the
serial reference control; do not reopen it as a kernel gap. The recorded
XZ encoder and filter RSS tradeoffs are accepted subject to boundedness
verification above. Accepted parallel extraction is not reopened just to
force aggregate CPU parity.

Passing SDK ratios also do not waive regressions against the preceding
product. The combined allocation cohort records T4 wall/CPU +5.22%/+5.96%,
T2 +3.49%/+4.24%, and L1/L3 T16 wall +5.31%/+4.83% against the table-only
port. Its zeros L1 T2 and L3 T16 RSS tradeoffs are +8.23%/+14.51%, with
substantial timing wins. These remain distinct from accepted ring and
parallel-extraction tradeoffs. The complete SDK BT port passed differential
checks but regressed several intermediate widths and high levels. The current
working candidate pairs that walk with the SDK consumer in the normal x86-64
assembly build; this is not a promotion, and broader-width regressions still
need resolution after T16. The preceding word-scan kernel remains available
for comparison. The range-local cohort also has preceding-product
regressions at L5 T4 and zeros T2; these remain open for attribution even
though their SDK timing ratios pass.

## T16 shared-path follow-up

Topology is a profiling hint, not an implementation policy. The encoder will
not select kernels or affinity from core type. On the same x86 diagnostic
binary, the retained complete SDK tree walk measured ordinary L5 T16 wall
+0.67% / CPU +2.92% and encrypted L5 T16 +0.78% / +1.43% against the official
SDK. Zeros T16 remained -1.58% wall / +4.78% CPU, so this is not closure and
not grounds to promote the diagnostic kernel. Peak RSS was within +0.25%
for all three cases. All warmups, rotations and outliers are retained.

Whole-path profiles identify tree walking, low-hash mixing and hash production
as the main ordinary-input costs. Hardware AES itself accounts for roughly
0.18 CPU seconds out of 110; zeros also spends several CPU seconds in the
SDK-shaped Skip family. The next diagnostic batch retains the complete SDK
walk structure with the existing bounded match extension, and checks input
spans once per held Skip segment. Neither changes topology policy. The
unmodified faithful SDK expansion stays available as an oracle.

Evidence uses decoded JSON SHA-256 values:

- `x86-t16-sdk-revisit.raw.json.zst`:
  `c7d28557cb0629413facfe432a810be8fa28e5dec893bbdee720b1a49dc3ee65`.
- `x86-t16-sdk-revisit.report.json.zst`:
  `bf832ef9dd97f6025cfb7becc1ebdf391aaf83fc87c2c0162bcf32622d1f2e3d`.
- `x86-t16-whole-path-profiles.raw.json.zst`:
  `f423fbe688e473554d94e7fb3ed18325309031ebd79dc913bba6734ebf6fac0e`.
- `x86-t16-whole-path-profiles.report.json.zst`:
  `c1464250ae888cdb05b7bdb8794e3f3519cb8456cc621f3a2c4f605889beb3f9`.

The retained-span/SDK-wide batch passed the complete native library suites
on x86-64 Linux (214 tests) and Windows (213 tests). An initial near-wrap
differential fixture violated the physical-history margin; the fixture was
corrected for both paths and strengthened to prove full consumption and an
actual window slide. The safety check and production code were not weakened.
Generated-code inspection confirms that MIN2/3 input bounds are checked at
span entry while BT cursor checks remain inside the loops.

The complete x86 T16 cohort for diagnostic binary
`139022e524055ffd35afd465bb5b998f955284cf57983755ec3dfbe4891f7410`
retains all 48 operations, full extracted-payload SHA checks, and identical
unencrypted product archives. These are partial results, not promotion of the
combined kernel or closure across architectures:

| L5 T16 case | Wall vs SDK | CPU vs SDK | RSS vs SDK | CPU vs frozen product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | +3.02% | +4.73% | +0.21% | -0.64% |
| AES | +3.65% | +4.22% | +0.09% | -7.48% |
| Zeros | -1.79% | -0.65% | +0.08% | -1.10% |

The faithful SDK, shipping, and combined kernels remain available. The safe
Skip span change is retained for continued evaluation. No individual gain is
discarded because another row misses; no cross-build gain is attributed solely
to Skip. Whole-path profiles compare all three kernels in one binary before
any further implementation choice. Windows zeros remains open and is being
profiled separately; the earlier ARM result belongs to its earlier source.

Additional decoded JSON SHA-256 values:

- `x86-t16-span-wide.raw.json.zst`:
  `71c998426f9071de5488513f5fcb402c0f6b65b1f8130aeac69a18d316f83235`.
- `x86-t16-span-wide.report.json.zst`:
  `5d161af05f67764074628ef97f0c1471716208c5f53d3f36c320f65affaa6031`.
- `x86-t16-span-wide-validation.raw.json.zst`:
  `1afdd33b4409356d9cbdc4de4b33d42b906031fbf56bf5e84b8643d308ae27d9`.
- `x86-t16-span-wide-validation.report.json.zst`:
  `7d943b56489ccffb7546d0f5ed3c60bfb24776a4d3a97deecfffd39120c56f0f`.

The subsequent 36 full-process profiles retain all three kernels and SDK,
three recordings per T16 input. Complete parser-family attribution includes
range coding, literals, prices and dispatch so inlining does not move work
out of the comparison. CPU-clock attribution remains diagnostic, separate
from uninstrumented acceptance time. The combined kernel's AES recordings
include two large outliers; neither is removed. Ordinary median CPU shares
remain approximately 52 seconds in BT, 22 in low-hash mixing, 21 in the
complete parser family and 8 in high-hash production. Zeros spends roughly
4.5 seconds in BT, 3.6 in Skip and 1.0 in high-hash production.

The whole-family source and generated-code audit identifies two candidates:
prove the high-hash output domain once at entry while preserving checked
fallback behavior, and restore the SDK's compact out-of-line low-hash
consumer with a position-relative history address. These are measured-path
candidates, not asserted wins. The consumer remains diagnostic; all three
tree variants, the previous product and full correctness oracles are kept.

- `x86-t16-span-wide-profiles.raw.json.zst`:
  `f095464d84bf0a1e7d6ddddc71eb70cbae988551f0025bc7d1037de1d8007722`.
- `x86-t16-span-wide-profiles.report.json.zst`:
  `52532769cd6fbfbff00f501e0dfb6b16ebb3c032d0ca28296227d4dcdf04da27`.

The complete high-hash/consumer cohort retains 72 operations, all extracted
payload hashes, and identical plaintext product archives. Native library
validation passes 217 tests on Linux and 216 on Windows. The diagnostic
Linux binary is
`86f37fc3ad4bee7a2a74d663ac31f6a0e38d360d4bbf26f93697610daaa0cb9d`.
The faithful SDK tree plus SDK consumer passes all three Linux T16 medians:

| L5 T16 case | Wall vs SDK | CPU vs SDK | RSS vs SDK | CPU vs frozen product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | +1.79% | +2.51% | +0.12% | +0.24% |
| AES | +1.67% | +1.61% | +0.11% | -2.88% |
| Zeros, 512 MiB | -2.40% | +0.38% | +0.09% | +1.97% |

This is diagnostic acceptance on that host, not a production-default or
fleet-wide claim. The wide-tree combination misses ordinary and AES medians;
those results and its zeros gain remain in the evidence. Portable Miri
verification is still pending. Earlier ARM
measurements are tied to their earlier source. The Linux zeros fixture here
is 512 MiB; Windows and earlier ARM use 256 MiB. Within each cohort the SDK
receives exactly the same fixture as the product, but these cases must not be
pooled as one cross-host corpus.

- `x86-t16-hash-consumer.raw.json.zst`:
  `4021bfd7591862ec62a4330adf77e8a57edfd0c650f9cbac1bef005e639adec8`.
- `x86-t16-hash-consumer.report.json.zst`:
  `144422e9c86c7cfcaaa6c6146b2a2be9e38166080af55f732965f4214cfcd0ac`.

The matching Windows diagnostic cohort is also complete: 72 operations,
all decoded hashes correct, and identical plaintext product archives.
It prevents a universal promotion of the Linux winner. The faithful SDK
tree/consumer misses CPU by 5.29% ordinary, 6.33% AES and 31.15% zeros.
The current tree/consumer passes ordinary (CPU +2.63%) and AES (+2.58%)
but zeros remains +18.03% CPU, despite -7.82% wall and -6.22% working set.
No core-type policy or input-size change is used to hide these misses.
Fixed production builds, without diagnostic selectors, completed 36 operations
per host, with all decoded hashes correct. The production configuration keeps
the original tree and consumer with the retained-span Skip and high-hash proof.

| Host / L5 T16 case | Wall vs SDK | CPU vs SDK | RSS / working set vs SDK | CPU vs frozen product |
| --- | ---: | ---: | ---: | ---: |
| Linux ordinary | +1.08% | +2.33% | +0.20% | -1.21% |
| Linux AES | +3.19% | +4.79% | +0.14% | -0.85% |
| Linux zeros, 512 MiB | -6.93% | -0.01% | +0.12% | -1.27% |
| Windows ordinary | -1.92% | -0.37% | -0.53% | See complete report |
| Windows AES | -0.05% | +2.10% | -0.84% | See complete report |
| Windows zeros, 256 MiB | -3.93% | +10.22% | -4.38% | +30.57% |

Linux AES and Windows zeros remain open. The Windows zeros CPU samples vary
widely; every sample is retained rather than replacing the cohort. Whole-path
profiles compare each fixed production binary with SDK before another source
change. Windows symbolization uses only the PDB matching its captured binary.
The faithful SDK tree and consumer ports remain available independently; the
combined diagnostic winner does not establish which component caused its gain.
All Miri work remains deferred to the final capstone.

The fixed Linux production profiles contain three recordings per input and
binary, all decoded hashes verified. Sampled CPU covers roughly 99.5% of
process CPU for ordinary/AES and 96% for zeros. The complete consumer family
remains a measured AES difference (22.42 versus 20.82 sampled CPU seconds),
with high-hash production 7.74 versus 7.22; hardware AES is only 0.20 versus
0.19 seconds. These are family medians, not additive process decompositions;
the complete per-run records and SDK outliers are retained. Source inspection
identifies the production `move_pos` symbol as the inlined Skip wrapper, so
its samples stay in the Skip family rather than unexplained parser work.
No source change follows solely from these sample differences.

Production evidence, decoded SHA-256:

- `x86-t16-production-controls.raw.json.zst`:
  `56b1c059779ef1742ad52c42f530e43914905c1da759cc1081e414e1a71b0167`.
- `x86-t16-production-controls.report.json.zst`:
  `ced302c7aea20b2eaf98a2d836090f70ef612c10a78534f5dc563539d7e98ed5`.
- `x86-t16-production-profiles.raw.json.zst`:
  `ee0b3fe5cb1244abdce678756af05672d8a763abe49c1bfafc6a391919272f0c`.
- `x86-t16-production-profiles.report.json.zst`:
  `e6138c1352e6e70f6f83510960d767367c5ed3ad0529f6f50337db9b53eac076`.
- `t16-bt-consumer-dispatch-audit.report.json.zst`:
  `6b8f1d4253cab418cd4e7162d49fb678730762b3deef162d973985d6600c5a08`.
- Sevenz `windows-t16-production-controls.raw.json.zst`:
  `96d48d0339bb8fb46484d5a3a3e39426485cc53921a7f9156ba93130f5f4aac7`.
- Sevenz `windows-t16-production-controls.report.json.zst`:
  `68e4cf80436eb59f7ce32a7570f144f0b142bf1318162e75f2921a7710475c25`.

The independent-combination cohort now includes faithful SDK BT with the
original consumer. That combination passes ordinary but misses AES CPU by
9.17%; SDK BT plus SDK consumer passes all three cohort medians. The zeros
SDK samples include large outliers (10.36–14.11 CPU seconds), so the apparent
24.62% zeros CPU advantage is not treated as an intrinsic optimization gain.
All 48 operations, decoded hashes and plaintext product identities are kept.

A fixed normal-feature candidate now selects the complete SDK tree and
consumer on x86-64 with `asm`, without runtime diagnostic selectors. Other
production architectures and no-assembly paths retain their previous selection.
The earlier kernels and consumer remain available for differential tests and
diagnostics. Native library validation passes 217 tests. This selection also
affects inline/T1 and other finder families, so broader regression gates remain
required before promotion. The candidate binary is
`31b6ec921b7602d4065b1cef12486dca91a22373fdc161c3e765bdd3f4702db4`.

| Initial fixed SDK candidate cohort / L5 T16 | Wall vs SDK | CPU vs SDK | RSS vs SDK | CPU vs preceding fixed product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | -0.25% | +0.30% | +0.06% | -2.54% |
| AES | +6.46% | +7.09% | +0.07% | -2.71% |
| Zeros, 512 MiB | -3.35% | +2.32% | +0.12% | +1.24% |

AES missed in this initial cohort. Its partial product improvement is retained,
along with every timing outlier; the subsequent profiles compared this exact
fixed candidate, preceding fixed product and SDK without changing implementation.

Those profiles show median consumer CPU falling from 22.14 to 20.70 seconds
(SDK 20.37), and whole-process CPU 99.66 versus SDK 99.28. These instrumented
times do not replace acceptance. A subsequent harness audit found that
reversing individual rotations had left the candidate never first and SDK
never last in the three measured uninstrumented runs. This order imbalance
does not establish the cause of any particular outlier, but prevents treating
that cohort as position-balanced. All original results remain above and in
the artifacts. The fixed source then completed one predeclared six-order
confirmation: P/C/S, C/S/P, S/P/C, S/C/P, C/P/S, P/S/C, with a separate warmup
(P preceding product, C candidate, S SDK). Each binary occupies each position
twice; all 63 operations completed, with no named compiler interference, full
decoded SHA checks, and identical plaintext product archives. This is the
balanced confirmation, not a replacement of the earlier observations:

| Balanced fixed SDK candidate / L5 T16 | Wall vs SDK | CPU vs SDK | RSS vs SDK | CPU vs preceding fixed product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | -0.94% | +0.41% | +0.24% | -3.47% |
| AES | +0.30% | +0.77% | +0.11% | -4.86% |
| Zeros, 512 MiB | -1.00% | +3.34% | +0.10% | -0.21% |

Zeros CPU remains open. An 18-record whole-path profile of these exact binaries
reproduces the gap: median process CPU 10.1824 versus 9.8505 seconds. Skip
accounts for 3.8789 versus 3.4094 seconds, tree walking 4.0375 versus 3.7222,
and heads 1.0360 versus 0.9610. Parser and CRC savings partly offset those
costs. Family medians are not additive and instrumented timings are diagnostic.
The hot tree long-copy loop already has the same 21 instructions, operands,
relative branch targets and alignment as SDK; equivalent CMP encodings differ.
The complete shared GetMatches/Skip cursor path is the remaining audited
representation difference: SDK advances a direct record pointer, while the
preceding consumer repeatedly loads and advances a word index. The new scoped
held view covers both GetMatches and Skip. It requires successful producer
acquisition, confines pointers to one published block and checks output room
before mutation. Synthetic records and short output retain the indexed product
fallback; valid-stream differential tests retain the independent checked path.
The generated normal-product loop now advances the direct record pointer.

Native validation initially passed 217 tests and failed one new short-output
test. That test incorrectly expected the checked oracle's partial mutation on
panic: the preceding product preflights output before changing low hashes.
The fast cursor was not entered in this case. A test-only correction compares
the unchanged indexed product fallback and passes both mixer variants; all
state and output assertions remain. That targeted correction did not itself
constitute a complete passing sweep. The initial failure and correction are saved.
The normal product build passes, with binary SHA-256
`76772594488dbf062d20bf9c5fd7155e90159773356a3d2c65b58b065eba1d84`.
Its complete balanced 63-operation comparison passes all three limits for
ordinary, AES and zeros. All decoded-byte checks pass; unencrypted product
archives remain byte-identical. All 124 recorded source hashes are unchanged
after the cohort, with no named compiler interference in 1,279 load checks.
Those checks do not establish an otherwise load-free machine. All warmups,
outliers and earlier failures remain in the evidence. Miri remains deferred
until the final capstone.

The subsequent consolidated native validation passes: lzma all-feature
workspace 429 tests (three ignored), sevenz all-feature workspace 504 (one
ignored), sevenz default AWS-LC 474, and lzma portable encoder without assembly
169; zero failures. Decoder no-std and encoder-only checks also pass. All
124 source fingerprints remain unchanged. These checks use the retained
26.03 C correctness oracle and do not establish Miri or ARM results.

| Shared-cursor / L5 T16 | Wall vs SDK | CPU vs SDK | RSS vs SDK | CPU vs preceding fixed product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | -0.46% | +0.01% | +0.25% | +0.02% |
| AES | -0.88% | +0.19% | +0.02% | -0.42% |
| Zeros, 512 MiB | -1.11% | +1.66% | +0.10% | -0.89% |

Zeros wall changes by +0.71% against the preceding fixed product; ordinary
and AES improve 1.23% and 1.08%, respectively. All preceding-product wall,
CPU and RSS comparisons remain within 3%. This closes these measured Linux
T16 rows, not unmeasured architectures or other thread widths.

The same source also passes the complete 63-operation Graviton4 Linux ARM64
cohort against the official ARM64 SDK 26.03 executable. Both product builds
retain the ARM defaults; this does not select the x86 assembly kernel on ARM.
The isolated preceding build differs only in the shared-cursor source file.

| Shared-cursor ARM64 / L5 T16 | Wall vs SDK | CPU vs SDK | RSS vs SDK | CPU vs preceding product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | -2.62% | -1.49% | +0.16% | +0.09% |
| AES | -2.22% | -1.73% | -0.03% | -0.10% |
| Zeros, 512 MiB | -27.07% | -27.14% | +0.05% | -0.66% |

The shared-cursor change improves ARM zeros wall time by 3.31% against the
preceding product; the much larger SDK difference is not attributed solely
to this change. Ordinary preceding-product wall changes by +0.33%, AES by
-0.08%, and every preceding-product CPU/RSS comparison remains within 3%.
All decoded SHA checks pass and unencrypted product archives are identical.
All 124 source fingerprints match the current local source and remain
unchanged after measurement; 515 compiler-interference checks are clear.

ARM native validation records 1,816 passing test executions across overlapping
feature configurations: lzma all-feature 428 and default 402, sevenz all-feature
504 and default 474, forced unthreaded Send-input five, and all three guard-page
tests including normally ignored cases. No-std and encoder-only checks and
both production builds pass. Ordinary-suite ignored counts remain recorded;
these totals do not imply 1,816 distinct tests or a Miri pass. Linux ARM results
do not establish Apple Silicon performance.

The candidate binary is
`bb027a2e299ad9c596325e8c448f90c314c671a6d6a245c30e5472fc2a91d996`;
the official ARM reference is
`9a26e7d54bfdae8a8f1750cdb70697547b738c2334aa89f3d3f2c8645c8443fe`.
The full evidence archive was retrieved and verified before instance
termination. All three temporary staging objects were removed. Runtime was
1.753 instance-hours through the termination request, 1.765 through confirmed
termination. Raw operational records remain ignored; committed evidence uses
role labels and sanitized paths.

Windows zeros also exposed a measurement discrepancy, not an allocator
hotspot. On the preceding normal production binary, a scheduling-only trace
with no sampled profiling or stacks measured interrupt-subtracted process
CPU within -0.06% of SDK, while `GetProcessTimes` still reported +8.76%.
Context-switch intervals, samples from the preceding diagnostic capture, and
cycle counts corroborate scheduled work; cycles do not replace CPU seconds.
There are no lost events or unmatched scheduling intervals. The only child
is the OS console host, excluded consistently from parent-process CPU.
Adjacent uninstrumented medians are wall -5.24% and working set -4.96%.
The charged-counter failure remains recorded. The subsequent fixed-SDK
candidate comparison applies the same scheduled CPU method to every arm and
all three workloads, with adjacent uninstrumented wall/working-set controls.
It retains all 126 operations: 63 controls and 63 scheduling-only captures,
including warmups and every one of the six measured order permutations.

The fixed-SDK Windows binary
`5071037d4ad3273e79312edbdbcb3f63093a347a45a559c1d1bea7d21cd18ae1`
passes these measured rows. It precedes the shared-cursor change; the
subsequent shared-cursor Windows confirmation is recorded below.

| Fixed-SDK Windows / L5 T16 | Plain wall vs SDK | Scheduled CPU vs SDK | Plain peak working set vs SDK | CPU vs preceding product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | +0.08% | +0.58% | -0.54% | +2.42% |
| AES | +0.15% | +0.99% | -0.66% | +2.46% |
| Zeros, 256 MiB | -2.23% | +1.58% | -3.64% | -2.40% |

All preceding-product comparisons remain within 3%; ordinary wall increases
2.65%, which remains recorded alongside SDK parity. This cohort's preceding
product zeros CPU is +4.08% against SDK and does not pass. The candidate's
conflicting charged zeros CPU is +40.39% in plain runs and +20.07% in captures;
neither counter failure is discarded or substituted for scheduled CPU.
Windows uses a 256 MiB zeros fixture, unlike the 512 MiB Linux/ARM fixture.

All 126 decoded-byte checks pass. Every capture has explicit zero event/buffer
loss, paired owned scheduling intervals, closed thread lifetimes, known
interrupt kinds and no malformed thread/stack records. One exported DPC has
an invalid entry timestamp but a valid end before ProcessStart and every
owned scheduling interval. Its original record is retained; removing that
noncontributing record produces exactly the same interrupt-overlap list.
An invalid timestamp that could affect owned execution still fails closed.
Final owned thread teardown remains counted through its matching switch-out,
including the observed 1.42 ms after ProcessStop; CPU is not clipped to the
process-stop event. All raw-sample medians were independently recomputed.

The subsequent current shared-cursor Windows binary
`774c5e9ca1a963fbf83a1ba5d96ada6ad30547989320336807fcea9d3a9397d0`
also passes the complete predeclared 126-operation cohort. Its preceding
product is the fixed-SDK binary above. The six measured orders, adjacent
plain controls and scheduling captures, parser and CPU calculation are
unchanged. All 126 decoded SHA checks and all 63 strict trace checks pass.

| Shared-cursor Windows / L5 T16 | Plain wall vs SDK | Scheduled CPU vs SDK | Plain peak working set vs SDK | CPU vs preceding fixed product |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | +0.13% | +0.92% | -0.52% | +0.58% |
| AES | +0.09% | +0.35% | -0.78% | -0.27% |
| Zeros, 256 MiB | -4.43% | -0.19% | -3.03% | -0.89% |

All preceding-product wall/CPU/working-set comparisons are within 3%.
The plain charged counters still report AES CPU +4.93% and zeros +5.38%
against SDK; those failures remain in the report alongside the consistent
scheduled-time calculation. Two pre-start interrupt records meet the existing
generic zero-contribution proof; exact interrupt-overlap lists are unchanged
when those records are omitted. No guard or CPU arithmetic changed to obtain
this result, and no sample was removed. Raw medians were independently
recomputed from the full retained data.

Lossless filesystem compression was applied only to postprocessing artifacts
after every timed operation finished. It retained the raw traces and XML,
with logical-byte hash verification for the four already-completed files
compressed later. This storage handling did not overlap any measurements.

Current Windows evidence is retained in the sevenz evidence directory. The
decoded JSON SHA-256 values are:

- `windows-t16-shared-cursor-controls.raw.json.zst`:
  `4a2b659d741773582bee5a701c343b4ba3fce4683f3340623df0ed51fead581a`.
- `windows-t16-shared-cursor-controls.report.json.zst`:
  `0009134638ea5ba6e81885cc744df377a74fe1d47ccf5a5a192dad9c7e3b4ce4`.
- `windows-t16-shared-cursor-controls.method.json.zst`:
  `dd3b0ab85b78bc4aeacc07b5a792cf46542fb2b94084e0f2ba92044ed525f5a6`.

- `x86-t16-independent-ports.raw.json.zst`:
  `735190863664d8fe5b46850cec984a5bd3b78776d28f9e85901eacebb7fe1d24`.
- `x86-t16-independent-ports.report.json.zst`:
  `35544a79c22031aaac46016cd1b2a7f0a5340447015e0753c53819c73f5afbb3`.
- `x86-t16-sdk-fixed-controls.raw.json.zst`:
  `2844e3f333f9a7994e3091c4b0e30b935071e5061888e61ef73c251ebec169c5`.
- `x86-t16-sdk-fixed-controls.report.json.zst`:
  `fc5febd29d169a731c9842f0ca0200132701557c93b79c39b9fe380882d19306`.
- `x86-t16-sdk-fixed-validation.raw.json.zst`:
  `87840eb80acf6d9864560ab680f85734117fe1d97ae6d32378fe5c4a60136882`.
- `x86-t16-sdk-fixed-validation.report.json.zst`:
  `43236a44a827b74a1c271dbbad04f2ce8047b1aea40a6fc2ab8c68a267419567`.
- `x86-t16-sdk-fixed-profiles.raw.json.zst`:
  `b83261b6929bfa533c0341dd1cd1fb4ba9314c1e85094362867d6b8853ad4d7b`.
- `x86-t16-sdk-fixed-profiles.report.json.zst`:
  `6a293068976ff70238c060d7a383acfb5ebfd19ee24d1ea4fc4fbd9d0017e4d5`.
- `x86-t16-sdk-balanced-controls.raw.json.zst`:
  `e635c95062352d0021c97ea4e0d09aef3408371c0a6b195bd078baeea5892bfd`.
- `x86-t16-sdk-balanced-controls.report.json.zst`:
  `5d2a8afa8b96cc54bdfcfe2167f5447d924f98d1236d6ca49ad5ad427f37390c`.
- `x86-t16-sdk-balanced-method.raw.json.zst`:
  `aef6471d6fbf7aaba63262fe839fba5aa3caca382732fc0da262afb23e4c79b2`.
- `x86-t16-sdk-balanced-method.report.json.zst`:
  `d36e261881a08f636210f8ab640c8b7adaa6bb8bb3a03073a404d46e14bc750b`.
- `x86-t16-sdk-zeros-profiles.raw.json.zst`:
  `2637519ada63bfbac5ec3414b8ecb3ad4b10825983739b6b3bbed3a7be372ae3`.
- `x86-t16-sdk-zeros-profiles.report.json.zst`:
  `dd67e7b83d1d79219e264ba2368a5f955c1ca577949a5b63022eed06dbb9d8f3`.
- `t16-skip-shared-cursor-design.report.json.zst`:
  `c22cf38f4067cd3f57c6342792ce97a2d26ab1979b86d7758e184462d25df136`.
- `x86-t16-shared-cursor-validation.raw.json.zst`:
  `f96a4be90099306df374f2ba40a71356876a4c1d45eab511a72324ef15656c49`.
- `x86-t16-shared-cursor-validation.report.json.zst`:
  `ffdc42be36a15df3a8f80fd653369f628894dae9fbceda3fbda6b198601e4139`.
- `x86-t16-shared-cursor-controls.raw.json.zst`:
  `d23fa8397d9511b109701c5102a1ab8115b661a57d9291b7f6889435131f78d2`.
- `x86-t16-shared-cursor-controls.report.json.zst`:
  `328399036742453564215f2c1dc75839368c344ddf3b80a8969ade9b43e2a480`.
- `x86-t16-shared-cursor-integration.raw.json.zst`:
  `7b347efea2f35b50b9f71e48e1c6d176f2b0e3517960efb2f256ffc854307cf4`.
- `x86-t16-shared-cursor-integration.report.json.zst`:
  `652fa56fceb5e64a9ee5305bc8898da6565990a622c4ab5b44d0c1dff87a8596`.
- `shared-cursor-contract-review.json.zst`:
  `6783ea5167e65af46e57223fafcd0c4bb6514013b267e640e185054769be9a1a`.
- `arm-t16-shared-cursor.raw.json.zst`:
  `4d0385158287a0f26259820d8a5a55d7913001325a1c5fbe61fffc42a8daad6a`.
- `arm-t16-shared-cursor.report.json.zst`:
  `62fbb9d607fc8cfcb49ffa19a9cd14dbc782b9cc077b4784c979e3cc93ae0421`.
- `arm-t16-shared-cursor.validation.json.zst`:
  `9a30588986b4267ad1ee631853ee1a398729af387b0b6158c327dfbd73f546db`.
- `arm-t16-shared-cursor.cleanup.json.zst`:
  `7cc2f297520434aa28b9d96a587fa90790d0e138f994c1d5605945c6e49977fc`.
- `arm-t16-shared-cursor.evidence.tar.gz` (archive SHA-256):
  `ebdd17734985d2d7a61e6a73336d2b59e50f51c598172bc03a4bedcc37050f92`.
- Sevenz `windows-t16-normal-final-accounting.report.json.zst`:
  `0d4a18168d3bccf8513c5bd336fb1c82b3ca3444f364269170a65ed869651d47`.
- Sevenz `windows-t16-normal-final-accounting.method.json.zst`:
  `778bdd4dc80562b86bcd579d5ef30f3c218cb47630a7881174335baf2d6401e5`.
- Sevenz `windows-t16-fixed-sdk-controls.raw.json.zst`:
  `98bc92241dedef3f849ac7b8b5c91f0845e56a3673651981a706ba478182084c`.
- Sevenz `windows-t16-fixed-sdk-controls.report.json.zst`:
  `59d09c55901fe8bc9d8ccb9205edacc694ef895b65d695cea05efa5890cbc598`.
- Sevenz `windows-t16-fixed-sdk-controls.method.json.zst`:
  `ed912b75d233c1c2e771bbe5ffa46d5952b2ef094060426486237202c57b72f2`.

The first Miri attempt passed the high-hash domain/fallback cases, then
found a test fixture mutating its owner after publishing the window pointer.
Both affected fixtures now initialize through the published pointer and
extent, preserving every byte, state transition and assertion. Only tests
changed. The initial failure is retained. The 26-test rerun was interrupted
at the operator's request after 646 seconds; it is incomplete, not a pass.
All remaining Miri work is deferred to the final capstone. Native-validated
production T16 comparisons continue independently.

Sevenz evidence, decoded SHA-256:

- `windows-t16-hash-consumer.raw.json.zst`:
  `f0fd63231ca8410127fe4ddd61d01975aeb84a3439ecf66bba7bd5e53a58acb4`.
- `windows-t16-hash-consumer.report.json.zst`:
  `266d6f80db1ea5845d105a0ac959dd63cc9d4f6f4b04f384ab7efc63865b03d0`.

## Decode buffers and scheduling

| Path | Ownership and reference comparison | Investigation |
| --- | --- | --- |
| XZ and raw LZMA2 single stream | The benchmark owns a 1 MiB input buffer and a 1 MiB destination. The library reader owns a 64 KiB input buffer, dictionary and probabilities. XZ without filters decodes directly into the caller's destination. xz's CLI uses 8 KiB input/output buffers on this platform. | The reproduced 11.3 versus 9.8 MiB RSS difference cannot be attributed to a hidden 2 MiB library buffer. Preserve the original measurement and distinguish caller configuration from product changes. |
| XZ filter chain | Adds a 64 KiB scratch buffer, converter staging and pending output. | Audit staging capacity and copies; retain the existing ARM filter wins. |
| Sevenz parallel LZMA2 | Uses the adaptive worker pool, not the separate SDK ring driver. Workers decode into their output buffer without a second dictionary. Input is shared in 256 KiB pieces. | Account separately for queued input, active outputs, completed outputs, recycled capacity and allocator retention. |
| Sevenz output spill | Limited drains normally bypass the spill path. Historical ledger rows report zero spill. | Removing the nominal eight spare slots alone cannot close the observed gap. |
| Sevenz ledger | Bounded vectors of counts and timings, a few KiB rather than a MiB per worker. | A measured ledger/no-ledger RSS difference does not prove a ledger allocation of that size; allocation timing and retention must be examined. |
| SDK multi-block decode | 256 KiB MT input pieces, one output buffer sized to the parsed run per slot. The output doubles as the dictionary. | Compare the number of live and retained run buffers, not just configured threads. |
| Tall single-thread fallback | Current reproduced Mac row is 147.3 MiB versus 155.9 MiB for the official oracle. | Control row. The reported Linux/Windows gap is outside this Mac pass. |
| Non-solid folder extraction | Ordered folder workers stage output; each owns its coder and source. The plan permits two staged folders per worker and 1 MiB output pieces. | Trace actual member sizes, resident dictionaries, stage peaks and source buffering before changing concurrency. Preserve the substantial wall-time gain. |

## Filters

The checked 26.03 `Bra.c` and `Delta.c` routines are scalar. A missing NEON
implementation is not established by the handoff. Compare complete reader,
converter, checksum and output paths before choosing an architecture-specific
kernel.

- Sevenz BCJ rotates its entire 64 KiB buffer at a refill boundary. The SDK
  filter reader moves only the unconsumed suffix. Check partial reads, tails,
  errors and all converter alignments when removing the unnecessary copy.
- ARM64 conversion checks each instruction and updates branch targets in
  place. Compare bounds checks and loop shape with the reference, preserving
  both BL and ADRP handling and wrapping program counters.
- Delta distances below sixteen use a runtime-distance byte recurrence.
  Distance one can carry its preceding value in a register. Any specialization
  must retain history across arbitrary splits and use the same byte arithmetic.
- BCJ2 scans the main stream bytewise, then copies the scanned span. The SDK
  unrolls four copy-and-test steps and retains only the necessary previous-byte
  state. Preserve input-versus-output exhaustion priority, RC normalization,
  partial target words and stream error behavior.

## Encoders

- Match-finder window, hash and son allocation sizes are derived from the SDK.
  xz uses a similar window but different reserve constants. Both avoid touching
  unused son pages. Existing direct-input paths must remain direct.
- XZ solid input is held to the dictionary threshold before streaming. The
  held input moves to the pipe. Filtered input currently creates a second
  converted allocation during that transition.
- The XZ pipe recycles input chunks but allocates new output messages. The
  writer takes encoder output and then drops its previous empty allocation.
  These cross-thread lifetimes can raise resident allocator memory beyond
  live buffer capacity; investigate recycling before reducing useful windows.
- Finite XZ blocks grow a `Vec` geometrically. A 24 MiB block can therefore
  occupy 32 MiB. Parallel batches retain input and compressed results, then
  append the whole batch to one output vector. Account for those simultaneous
  allocations and the spare input buffer.
- Sevenz's bridge already recycles input and compressed output. A whole block
  changes ownership at `write_vec`; it is not copied into another block.
  Inspect the number of replacement buffers retained while a block is being
  written, together with the SDK block-slot count.
- Sevenz sizes dictionaries to known folders and reuses matching single-thread
  coders. The Mac fleet non-solid tree size difference is 171,720,103 versus
  171,682,744 bytes (about 0.022%), not the 6–7% reported on other platforms.
  Separate packed-stream differences from archive metadata before changing
  encoder properties.

## Allocation findings and candidate changes

The two-thread near-incompressible decode snapshot held 4.68 MiB of live
allocations but 11.1 MiB of resident malloc regions. Allocator fragmentation
accounted for 6.4 MiB; stacks occupied 128 KiB. The input pool returned only
about half of the pieces consumed across the stream. Its old retention cap
held half of one run at two threads. The candidate retains completed input
for a worker wave, still charged inside the existing memory bound. Dispatch
can shed that spare capacity before refusing work.

The non-solid single-thread snapshot held 4.0 MiB in 41,194 allocations,
including the public per-folder metadata. Malloc residency was 5.72 MiB.
There was no extra 1 MiB input buffer on this path. Changing public metadata
representation or dropping parallel workers has API or throughput costs;
neither is justified by the original buffer hypothesis.

The grouped candidate also:

- compacts only the live BCJ reader suffix;
- copies and tests BCJ2 main-stream bytes in the SDK's four-byte loop;
- keeps the previous delta byte in a local for distance-one decoding;
- caps XZ block input growth at its boundary and reuses completed batch inputs;
- reserves aggregate XZ batch output without geometric excess;
- recycles pipe and writer output allocations;
- converts large filtered XZ writes through bounded scratch;
- returns each sevenz whole-block output allocation to its original SDK slot
  after delivery, including disconnect handling on failure and abandonment.

## Initial Mac validation

Formatting and all-target, all-feature Clippy pass in both workspaces. The
lzma native suite passes 377 tests; two explicitly ignored guard-page stress
tests remain unrun. Exact 26.03 SDK oracles and external xz/7zz checks were
enabled. The sevenz default-feature suite passes all 400 tests. Its initial
all-feature sweep passed 414 of 415 tests: the EOF/refusal test decoded
correctly but no longer reached refusal under the improved buffer reuse.
It now constructs the exhausted-source/refused-tail state explicitly, retains
the refusal assertion, adds an EOF-deferral assertion, and passes under both
feature configurations. No sleep, timeout or retry was added.

Completed fresh decode baselines cover 16 lzma and 18 sevenz scenarios.
The encoder baseline was interrupted after the xz control drifted from
roughly 51 to 78 seconds under unrelated host activity. Its partial evidence
is retained and is not an acceptance result. The second Mac decode candidate
still missed the eight-thread wall gate and several RSS gates. Its evidence
is retained; it does not establish acceptance. Subsequent measurements moved
to an x86-64 host while the Mac was busy.

## Second pass: missing paths and x86-64 findings

The native ARM64 decoder kernel is already enabled by the default `asm`
feature. The encoder previously selected the register walk only on x86-64.
The candidate now selects its portable pointer implementation on ARM64 too,
through the normal encoder path. The exact SDK has no ARM64 encoder assembly
to transplant. This closes the missing dispatch, not every performance gap;
the archive-path measurements and remaining full-thread deficit are below.

The SDK's `Bra.c` and `Delta.c` are scalar, including ARM64 and Thumb; the
handoff's claim of SDK SSE/NEON versions was not borne out by source. CRC
and default crypto already use their dependency's architecture kernels.
The existing eight-byte match-length kernel is active; the earlier report's
sixteen-byte alternative lost whole-encoder performance and is not enabled.

The second grouped candidate adds:

- single-word endian reads and fixed-width instruction slices for BCJ;
- the SDK's native-endian PPC predicate and rotate-based SPARC formulation;
- four independent delta accumulators for distance four;
- an eight-byte candidate scan for BCJ2, followed by the original scalar
  validation, including candidates across word boundaries;
- in-place XZ filter stages over the block decoder's owned pending output,
  retaining only the short unconverted tails;
- private inline storage for common sevenz folder metadata and a validated
  one-coder path that avoids temporary graph allocations;
- an adaptive output-recycling fix applying the existing one-buffer-per-slot
  rule to every buffer size. A one-MiB run equals the chase step size, so the
  previous greater-than-step exception retained surplus output and consumed
  input room. The new regression fails on the previous implementation and
  passes after the fix, with small, one-MiB and four-MiB runs;
- shared zeroed dictionary allocation, preserving initialized Rust values and
  recoverable allocation failures. This removes the eager full-dictionary
  write while startup input is still resident. On the measured x86 single
  stream, this reduced peak RSS from 151.5 to 127.4 MiB; read-ahead policy
  is unchanged.

The subsequent x86 filter pass uses candidate scans for ARM64 and SPARC to
avoid LLVM's speculative vector conversion of words that are not branches.
The reference xz ARM64 implementation explicitly disables this vectorization
under Clang. Thumb uses a checked word predicate, with an exhaustive
halfword-pair differential test. The XZ converter buffer now keeps initialized
storage separate from valid output, avoiding a full zero-fill before each
decoder write. Carry space is reserved without geometric growth. The next
candidate writes new input after the carried prefix and lets each stage fill
its own prefix space, eliminating the full-chunk shifts as well. Multi-stage
tail/boundary differential coverage passes; x86 verification is queued after
the current measurements.

A further single-stream allocation audit found the input spare allowance
still sized for all configured workers while the calling thread alone chases
one run. The regression reproduces two MiB of retained pieces after two MiB
of output at a sixteen-thread ceiling. The candidate limits that serial case
to one piece and one refill, including releasing a previous wave's excess.
All 24 adaptive unit tests pass after the initial cap change; final x86
validation and RSS/CPU measurement remain pending. Admission, thread limits
and the parallel wave's reuse allowance are unchanged.

Sevenz inline header storage now applies only on 64-bit targets. Narrower
targets retain boxed storage so inline fields cannot enlarge every folder.

### Completed x86 filter measurements

Three interleaved repetitions and one warmup, same corpus and xz 5.8.3.
Percentages are `(ours / oracle - 1) * 100`; negative is better. These results
precede the shared decoder allocation change. Raw records and reports are
under `mac-fleet-evidence/x86-*.json.zst` with neutral machine labels.

| Filter | Baseline wall | Candidate wall | Candidate CPU | Candidate RSS |
| --- | ---: | ---: | ---: | ---: |
| x86 | -1.82% | -3.50% | -3.46% | +26.62% |
| ARM64 | +5.88% | +4.95% | +4.95% | +24.37% |
| ARM | +4.95% | +1.06% | +1.01% | +28.80% |
| Thumb | +2.97% | +5.09% | +5.09% | +27.80% |
| PPC | +4.89% | -0.05% | -0.11% | +26.71% |
| SPARC | +5.89% | +6.22% | +6.26% | +28.42% |
| IA-64 | +1.60% | +0.75% | +0.69% | +28.86% |
| RISC-V | +7.64% | -1.17% | -0.26% | +27.58% |
| Delta 4 | -3.68% | -5.07% | -5.07% | +25.54% |
| Delta 64 | -42.49% | -57.52% | -57.59% | +28.11% |

ARM64, Thumb and SPARC still miss the timing gate. Filter RSS remains above
xz: the harness's one-MiB input and output buffers are part of its measured
process, unlike xz's smaller CLI buffers. The product staging reductions are
real, but do not justify reporting RSS parity or changing only the probe to
claim a product improvement.

The sevenz x86 baseline did not reproduce the reported ARM64, delta or BCJ2
timing gap against the matching-thread oracle. BCJ2's ordinary 7zz control
uses another filter thread; both that control and the parity control remain
in the evidence.

### Completed x86 sevenz decode measurements

The same default-feature candidate, before shared zeroed allocation, passed
the three-percent wall and CPU gates in all twelve selected scenarios.
BCJ2 uses the matching-thread control below. The complete raw/report pair is
in sevenz's `docs/mac-fleet-evidence/x86-candidate-sevenz-decode.*.json.zst`.

| Scenario | Wall delta | CPU delta | RSS delta |
| --- | ---: | ---: | ---: |
| st Tall | -0.31% | -0.30% | +22.92% |
| media Tall | +0.90% | -0.05% | +3.98% |
| media T2 ledger | +1.40% | +0.81% | +6.04% |
| media T2 no ledger | -0.68% | -1.34% | +2.92% |
| media T4 ledger | +0.97% | +0.23% | +1.30% |
| media T4 no ledger | +0.54% | -0.10% | -0.05% |
| media T8 ledger | -4.17% | -0.23% | -2.49% |
| media T8 no ledger | -2.66% | +1.15% | +2.02% |
| AES T2 ledger | +0.18% | -0.16% | -0.56% |
| AES T2 no ledger | +0.55% | +0.26% | -1.43% |
| AES T4 ledger | -0.50% | -0.65% | -4.50% |
| AES T4 no ledger | -0.22% | -0.38% | -5.17% |
| AES T8 ledger | -3.32% | +0.65% | -2.66% |
| AES T8 no ledger | -5.04% | -0.62% | -4.90% |
| AES Tall | +0.15% | -2.24% | -1.95% |
| ARM64 T1 | -5.45% | -8.55% | -3.61% |
| BCJ2 T1 | -1.37% | -1.34% | -7.17% |
| Delta T1 | -1.63% | -4.18% | -4.31% |

RSS still misses the gate on the single-stream control, media Tall and media
T2 with ledger. The actual `st.7z` header reports `LZMA2:25`, a 32 MiB
dictionary. Its 128 MiB expected run size is four dictionaries; the handoff's
description of this archive as having a 128 MiB dictionary was incorrect.
It contains one GiB of output and 937,889,804 packed bytes. Eager dictionary
initialization overlaps the startup input queue. The shared zeroed allocator
subsequently measured 127.4 MiB against 123.2 MiB, with wall and CPU both
1.15% faster. RSS is still 3.39% above the oracle, so this is an improvement,
not acceptance. The complete second comparison is preserved in sevenz's
`x86-lazy-sevenz-tall.*.json.zst` files.

### Follow-up filter measurements

The candidate-scan and initialized-buffer runs are separately retained as
`x86-scan-lzma-filters.*.json.zst` and
`x86-buffer-lzma-filters.*.json.zst`. Both use three interleaved repetitions.

| Filter | Scan wall | Buffer wall | Buffer CPU | Buffer RSS |
| --- | ---: | ---: | ---: | ---: |
| x86 | -3.67% | -3.56% | -3.47% | +27.40% |
| ARM64 | +1.86% | +4.84% | +4.86% | +25.66% |
| ARM | +0.53% | -0.58% | -0.60% | +28.76% |
| Thumb | +4.94% | +0.77% | +0.69% | +27.19% |
| PPC | +2.92% | +0.55% | +0.58% | +29.00% |
| SPARC | +2.61% | -0.05% | -0.12% | +27.33% |
| IA-64 | +0.33% | -0.18% | -0.27% | +27.21% |
| RISC-V | -0.93% | +1.43% | +1.42% | +26.97% |
| Delta 4 | -5.51% | -5.61% | -5.62% | +28.64% |
| Delta 64 | -56.91% | -65.41% | -65.46% | +27.27% |

ARM64 varies across the threshold; no all-filter acceptance is claimed.
These short runs need a longer confirmation. The buffer run precedes the
final exact carry-capacity reservation, which remains to be timed.

### Revised five-host scope

The operator's completed-fleet update at 2026-10-13 15:44 UTC reports no
primary or secondary failures on any host. Its lane changes are incorporated
without treating a baseline result as candidate validation:

- Full-thread encoder and streaming-reader timing gaps are Alder Lake only.
  Zen 4's corresponding baseline rows meet parity.
- Sevenz ARM64 filter decoding needs verification on ARM and Zen 4; Alder
  Lake parity does not establish that cross-host result.
- Near-incompressible Tall scheduling is worst on Zen 4, with the same
  CPU-parity/wall-loss signature. Mac ledger RSS remains a Mac-only lane.
- The single-stream startup RSS excess is confirmed across x86 hosts;
  encoder working-set gaps affect every architecture.
- Non-solid extraction RSS remains relevant on Mac and Zen 2. A substantial
  wall-time win can justify excess RSS under the operator's stated tradeoff.
- Budget/adaptive policy rows and the slower secondary decoder's lower-RSS
  tradeoff are excluded from defect scope.

The completed x86 encoder/streaming batch contains 22 scenarios, with no
primary or secondary correctness failures. Its broad Tall-decode selector
also included budget/adaptive controls; those samples are retained but are
not acceptance defects. Relevant medians, percentage over official 7zz:

| Scenario | Wall | CPU | RSS |
| --- | ---: | ---: | ---: |
| mt Tall | +0.36% | -0.53% | +0.01% |
| mt Tall streaming | -0.71% | -1.28% | 0.00% |
| solid tree Tall | +0.35% | +0.26% | +0.15% |
| solid tree Tall streaming | -1.40% | -1.99% | +0.15% |
| payload L1 Tall | -13.98% | -14.66% | +22.24% |
| payload L3 Tall | -8.75% | -9.73% | +10.14% |
| payload L5 T4 | -6.59% | -2.78% | +1.10% |
| payload L5 T8 | -6.86% | -1.46% | +0.43% |
| payload L5 Tall | +3.39% | +4.89% | -0.36% |
| payload L5 Tall AES | +6.57% | +7.74% | -0.24% |
| non-solid tree L5 T1 | -1.36% | -1.39% | +40.30% |
| non-solid KDF tree L5 T1 | -17.55% | -17.56% | +13.29% |
| non-solid KDF tree L5 T1 AES | -16.37% | -17.00% | +28.23% |

The raw archive-size ratios are not all exactly one: payload differences
are a few header bytes, with tree/KDF ratios from 0.9969 to 0.9997. Every
archive passed the oracle integrity check. Bit-exact encoder-stream output
is checked separately against SDK C. The sevenz evidence directory retains
`x86-candidate-sevenz-encoder-stream.{raw,report}.json.zst`.

The subsequent source audit found that streamed LZMA2 block workers using
the single-threaded finder copied their already-owned input into a second
window. The candidate now transfers that vector into the finder and returns
it after coding, including write errors. Borrowed callers keep the existing
path, as do custom blocks larger than 1 GiB. Memory estimates stay
conservative. This uses safe ownership and adds no unsafe block.

The completed owned-input batch passes all three gates on x86: st Tall is
-0.05% wall, -0.06% CPU and +0.44% RSS; L1 Tall is -12.10%, -14.77%
and +2.38%; L3 Tall is -8.22%, -10.83% and -1.00%, respectively.
The corresponding RSS medians are 123.8, 252.8 and 869.0 MiB. Evidence is
`x86-owned-sevenz.{raw,report}.json.zst` in the sevenz evidence directory.

The five-repetition carry-prefix filter batch against the pinned xz build
passes wall and CPU for all ten filters. The largest timing deficit is
Thumb, about 2.3%; ARM64 is about 1.6% behind. RSS remains 26–29% higher.
`x86-prefix-pinned-lzma-filters.{raw,report}.json.zst` preserves this run.
The separate `x86-prefix-system-xz` evidence used the system xz binary,
also version 5.8.3 but a different build; it must not replace the pinned
comparison or be combined with its samples.

### Resumed Mac measurements

After the operator reopened the Mac, 16 lzma and 17 sevenz scenarios completed
with three interleaved repetitions and one warmup, with no primary or
secondary failures. `mac-current-lzma` and `mac-current-sevenz` compressed
JSON retain every sample. These binaries include the owned-input and
carry-prefix changes, but predate the header and ledger snapshot fixes.

Mac L1 Tall is 12.2% faster with 13.0% less CPU and 2.2% higher RSS;
L3 Tall is 16.3% faster with 14.9% less CPU and 2.3% higher RSS.
ARM64 BCJ T1, BCJ2 T1 against the single-thread parity reference, and delta
T1 also pass all three thresholds. Near-incompressible media and AES decode
pass wall and CPU at full fan-out, but RSS remains 78.7 versus 46.3 MiB and
69.2 versus 46.7 MiB respectively. Ledger T2/T4/T8 RSS remains open.
Non-solid parallel extraction retains its roughly 13x wall advantage with
higher CPU and RSS; sequential and streaming non-solid RSS also remain open.

The first same-binary ARM encoder A/B compared the portable register walk
with the slice walk. Seeded L5 T2/T8/T18 and L9 T2 improved wall by 7–24%,
but zeros L5 T2 regressed 5.9% wall and 3.6% CPU. That dispatch was therefore
not enabled. A subsequent portable long-match change copies each tree node
as one unaligned 64-bit value and folds the output threshold into the head
bound. Native differential tests and Miri pass. Its second A/B retains
6.4–21.9% seeded wall improvements; zeros T2 is now +3.54% wall and +1.65%
CPU. These percentages compare against the existing Rust slice walk, not
against SDK C; they are an additional regression check, not the reference
acceptance ratio. A further distance-one repeated-node change removes its
store-to-load dependency and passes native and Miri checks. Its diagnostic
A/B still shows about +3.6% zeros T2 wall and +1.5% CPU. Background activity
and timing drift prevent using that run to approve the production dispatch.
The raw samples remain in `arm-tree-repeat-node-diagnostic`.

A subsequent same-binary comparison exercises sevenz's actual archive writer
and official 7zz with matching LZMA2 settings. All archives pass integrity
checks, and the register and slice variants produce identical archive bytes
in every case. The initial attempt caught a fixture access-time change in
the first warmup header; reading the newly created fixture before capturing
metadata fixes that setup issue without changing its bytes. Both the initial
diagnostic and the corrected run are retained.

| L5 case | Register vs slice wall | Register vs 7zz wall | CPU vs 7zz | RSS vs 7zz |
| --- | ---: | ---: | ---: | ---: |
| zeros T1 | +0.83% | -69.64% | -69.68% | +1.37% |
| zeros T2 | +7.92% | -35.21% | -27.71% | +2.08% |
| zeros T4 | +4.35% | -33.75% | -30.85% | +0.92% |
| zeros T18 | -1.34% | -18.08% | -20.63% | +3.85% |
| seeded T1 | -5.18% | -55.10% | -55.09% | +1.38% |
| seeded T2 | -17.29% | -15.82% | -10.61% | +2.35% |
| seeded T18 | -5.20% | +5.09% | +4.11% | +3.04% |

The candidate enables this walk in the normal ARM64 dispatch. The zeros
T2/T4 tradeoff relative to the previous Rust walk is explicit; those rows
still beat the reference substantially. The zeros T18 RSS excess accompanies
a substantial timing win, within the operator's permitted tradeoff. Seeded
T18 remains outside the thresholds and is not accepted. Host timing drift
also calls for confirmation of tight margins. Sevenz's evidence directory
contains `arm-archive-ab` and `arm-archive-ab-t1`.

### Header and ledger allocation audit

The x86 non-solid trace locates the encoder RSS peak at final header coding:
20.9 MiB during member compression rises to 30.7 MiB when a second encoder
thread starts. Sevenz now uses its existing caller-thread pull coder and
releases serialized metadata first. Plain and encrypted header bytes and
intermediate coder sizes match the previous chain in a differential test.
The measured tree peak falls to 29.1 MiB versus 21.8 MiB for 7zz; this is a
partial improvement, not acceptance. `x86-header-sevenz` retains the results.
The SDK header uses BT2 with 273 fast bytes; the current writer uses BT4 with
64. No header property change has been made.

Ledger publication previously called derived `Clone::clone_from`, replacing
three history vectors at every sampled read/drain. The buffer-identity test
fails before the fix and passes with explicit history reuse; snapshot values,
open waves and the 256-wave cap remain unchanged. This removes allocation
churn. The completed `mac-ledger-reuse` batch passes all three gates at T2
and T4 for both media and AES. At T8, RSS is still +19.0% for media and
+12.6% for AES; at full fan-out it is +51.3% and +59.1%. Timing passes all
eight scenarios. Separate heap snapshots show 37.1 MiB of live allocations
in 55.5 MiB of resident malloc regions at full fan-out. Moving worker-table
construction to the caller did not close that gap: the subsequent diagnostic
batch reports 77.1 MiB media and 74.1 MiB AES full-thread RSS, against 46.3
and 46.7 MiB. The batch overlapped background activity and is retained as
`mac-worker-tables-diagnostic`, not an acceptance run. A separate live-heap
comparison finds about 37.1 MiB allocated in 55.7 MiB of resident heap for
the candidate, versus 42.9 MiB in 43.3 MiB for 7zz. Thus less live data still
occupies more resident heap; initial probability-table placement alone does
not explain the fragmentation. Worker admission and budgets are unchanged.

The low-hash load-order candidate also leaves the x86 full-thread encode
lane open. T4 and T8 pass wall, CPU and RSS. Full-thread L5 is +4.3% wall
and +5.5% CPU; AES is +5.5% and +7.2%. RSS passes both. Samples are retained
in sevenz's `x86-mixer-sevenz` evidence.

### x86 validation and limits

The native workspace sweep, excluding the separately configured wasm
conformance runner, passed 383 tests with two ignored guard-page stress tests.
Exact SDK C, xz and official 7zz oracles were enabled. Sevenz's all-feature
sweep passed 414 tests with one ignored doctest. Formatting and all-target,
all-feature Clippy passed in both trees. These fresh broad runs include the
output-slot and shared-allocation edits. Shared allocation also passed its
Miri zero-value/ownership test, native decoder allocation-failure injection
and the `--no-default-features` build. The subsequent x86 filter scans passed
18 filter unit tests and 24 SDK/filter-encoder tests, plus formatting and
all-target, all-feature Clippy. The initialized-buffer change subsequently
passed the full native sweep and Clippy; the final carry reservation passed
the multi-stage boundary test and Clippy before rebuilding both consumers.

An earlier validation attempt reused cached binaries because a source archive
was extracted with normalized old modification times. Those `lazy-*.log`
native test/build results are not evidence for the allocation change. The
source was extracted with fresh times and explicitly recompiled; the
`fresh-*.log` runs above include the new allocation-refusal test. No timings
were made from that stale validation attempt.

The owned-input, serial-spare and carry-prefix candidate passed 386
native lzma tests with two ignored stress tests, 414 sevenz tests with one
ignored doctest, both all-target/all-feature Clippy gates and the core/alloc
build. The owned-input byte/ownership/error/reuse differential also passed
Miri and is included in the required Miri CI job. Narrow-target header storage
was separately compile-checked with layout assertions for wasm32 and i686;
that is not a substitute for the full wasm CI suite.

The subsequent Mac kernel sweep passed 387 tests with two ignored stress
tests. Its initial invocation omitted `LZMA_SDK`, so optional SDK C checks
silently skipped; that invocation is not evidence for C parity. The complete
sweep was then rerun with both `LZMA_ENCODER_ORACLE_REQUIRE=1` and
`LZMA_ORACLE_REQUIRE=1` and the pinned SDK directory: all 387 tests passed
without missing-oracle warnings. The corresponding required-oracle sweep
also passed on x86. All six native tree-kernel differential tests passed;
Miri passed the repeated-node and output-boundary cases and low-hash mixer.

### Follow-up allocation audit

Minimum output shedding now drops only the spare capacity needed by the
waiting run, rather than every spare except the next reusable one. Its
regression test first failed on the old behavior and then passed. The Mac
eight-scenario media/AES sweep still has a full-thread RSS gap: about
74 MiB versus 46–47 MiB. The corresponding x86 primary rows meet the 3%
limits; secondary no-ledger media T2 and T8 RSS remain roughly 4.6% and
4.9% above reference. Both complete batches are preserved as
`mac-minimal-shed-sevenz` and `x86-minimal-shed-sevenz` in sevenz's evidence.

The next input-pool audit found two sources of unnecessary allocation:
LIFO reuse could select a cut tail while full read buffers remained parked,
and tails filling the slot limit could cause a returned full buffer to be
discarded. Reuse now selects the largest spare, and a larger returned buffer
can replace one smaller spare within the same byte and slot limits. Both
regression cases failed before the change and passed afterward. Diagnostic
allocation counters are feature-gated and do not alter normal builds.

The normal ARM encoder sweep is preserved as `mac-arm-default-encode` in
sevenz's evidence. T1, T2, T4 and T8 meet the wall/CPU/RSS limits. T16 wall
is about 3.2% over reference; full-thread CPU is about 3.6% over and RSS
about 3.04% over. The full-thread allocation exceeds T16 by almost exactly
32 MiB. Source inspection found an eagerly zero-filled input block for a
worker that only discovers EOF. Fresh blocks now use the existing lazy
zero-initialized allocation helper. Sevenz also caps known-size folders at
their block count, as the SDK does, without setting the hash-size hint or
changing block boundaries. A differential test checks identical bytes at
exact and partial boundaries and when the stream exceeds its size hint.
The completed `mac-pool-eof-sevenz` batch measured these changes: full-thread
L5 RSS is 1170.6 versus 1167.9 MiB, a 0.23% excess, with wall +0.22% and CPU
+0.99%. The extra EOF block's resident pages are gone. This is not a clean
encoder matrix: T8 wall/CPU are +7.43%/+7.57%, T16 +3.38%/+3.19%, and
full-thread AES +3.48%/+3.71%. T4 RSS varies by one compressed output block;
the retained output freelist and synchronous return path match the SDK, and
both implementations sometimes allocate that additional block. All 13
scenarios completed without correctness failures. The corresponding x86
12-scenario batch has all eight primary decoder rows within 3%; full-thread
L5 CPU remains +5.68%. Both complete batches are retained.

An intermediate output-recycling change kept a just-emitted buffer only when a
complete pending run can immediately use it within the existing ceiling.
One x86 diagnostic reduced output allocations from 251 (251 MiB) to 49
(49 MiB), with the same 32.3 MiB accounted peak. This is an allocation
diagnostic, not proof of RSS acceptance. A Mac heap snapshot still found
37.2 MiB live in 58.0 MiB of allocator-resident memory, against the SDK's
42.9 MiB live in 43.2 MiB resident. The remaining issue spans scheduling,
ordered output delivery, input admission and allocator retention.

The lzma all-feature sweep after that recycling change passed 394 tests with
two skipped stress tests, including required SDK oracles and wasm
conformance. Sevenz passed 423 tests; nextest also reported one leaked-output
handle warning on the worker/chase test, which remains under investigation.
Formatting and all-target/all-feature Clippy passed on ARM and x86. Required
SDK native tests passed on x86 as well.

The completed `x86-immediate-reuse-sevenz` cohort has all 14 primary and
no-ledger comparisons within 3% for wall, CPU and RSS, with zero failures.
That is the x86 baseline for the subsequent lifecycle work, not evidence
for a later binary.

The next audit followed the entire decoder allocation path:

| Stage | Ownership and finding | Retained change |
| --- | --- | --- |
| Read and admission | The reader owns its in-progress piece; whole owned feeds are charged by capacity. A job can finish after that piece was acquired. | Keep one returned input piece across this handoff, still charged and capped by the caller's explicit limit. Input admission itself retains the existing pair bound. |
| Worker dispatch | The caller allocates output before dispatch; workers use it as their dictionary. | Count completed and partially delivered output as occupied slots, matching `MtDec_ThreadFunc2` through ordered `Write`. |
| Completion and ordered delivery | Results return input references and output; partial reads retain the same output allocation. | Reuse a slot only after its output is fully delivered. A deterministic test verifies partial delivery, refusal of excess dispatch, and pointer identity on reuse. |
| Recycling and shrinking | A lowered thread ceiling can leave outputs from the former width. | Release surplus buffers as they are delivered; preserve the caller's hard ceiling even for the input handoff allowance. |
| Allocator retention | Earlier input-buffer eviction repeatedly freed a whole 256 KiB piece to recover a smaller fractional excess. | Retain the handoff piece instead of cycling it through the allocator. |

Capacity-rounding, early completion collection during input reclamation and
reduced fixed-width read-ahead were also tested diagnostically and rejected:
they did not close RSS. The existing read-ahead policy and adaptive widening
are retained. Temporary lifecycle tracing was removed from production code.

The retained slot/handoff combination decoded 1,024 runs using 18 output
allocations and shedding only 0.5 MiB of input, with 36.5 MiB accounted peak.
Its Mac heap snapshot measured 37.1 MiB live in 37.9 MiB resident (2%
fragmentation), against 42.9 MiB live in 43.3 MiB resident for 7zz. These
snapshots are diagnostics; normal-build wall/CPU/RSS cohorts are required
before acceptance. The updated lzma all-feature suite passed 395 tests with
two skipped stress tests, including the required SDK and wasm checks.
Sevenz's subsequent all-feature sweep passed all 423 tests without the
earlier output-handle warning. That does not establish the warning's cause.
Both workspaces passed formatting and all-target/all-feature Clippy on ARM.
The required-oracle native sweeps passed on x86; its sevenz external-oracle
targets were explicitly rerun with the official 7zz directory on PATH.

Two later Mac timing batches were stopped when unrelated compiler load
appeared. The lzma encoder batch's reference p1 samples varied from 19.0 to
37.9 seconds; the decoder batch's media T2 reference varied from 8.7 to
18.6 seconds. Their logs and partial results are diagnostic only. An earlier
encoder invocation was refused by the source-fingerprint check before any
measurement. None of these batches establishes acceptance.

The first x86 slot/handoff timing cohort was likewise interrupted when an
unrelated compiler started during media T4: candidate samples rose from
7.0 to 9.3 and 11.6 seconds. Its partial data is diagnostic only. The frozen
default-feature binary is being measured again in a separate cohort.
That attempt was also interrupted by compilation during media T2. The next
cohorts monitor compiler activity and stop on interference; the Mac monitor
also checks for heavy VM activity. Interrupted cohorts retain their own
labels and do not contribute acceptance medians.

The non-solid extraction audit covers source cursors, per-folder decoder
construction, output staging, ordered callback delivery, checksum completion,
and cancellation. Each message currently wakes all workers through one
condition variable, and each output chunk is freshly allocated. These are
hypotheses for the remaining CPU cost, not yet demonstrated causes. Any
change must preserve per-slot memory bounds, callback and checksum error
order, panic cleanup, and a widened thread ceiling taking effect on message
activity. Short-process Mac attachment sampling missed the parallel run;
the single-thread sample was dominated by the existing ARM decoder kernel.
Those diagnostic samples do not establish parallel scheduling overhead.
The subsequent x86 launch-time profile captured 106,000 user-cycle samples
with none lost. Decoder assembly labels account for 97.30% on the efficiency
cores and 97.92% on the performance cores. This does not support a queue
rewrite as the primary CPU fix. Thread-width and mixed-core effects still
need controlled CPU-time measurements; the loaded-host profile is diagnostic.

The full-thread encoder profile also keeps the investigation in the encoder:
49.60% of sampled user CPU is in `sdk_walk`, 24.05% in `get_matches_kind`,
16.22% in the parser, and 7.51% in hash production. The official binary's
hot addresses were checked against disassembly to identify its low-hash
mixer and hash-production loops. Profile sample counts under background
load are not performance ratios. The shared-state audit found the SDK's
`kMtCacheLineDummy` (128 bytes) between ownership regions; the Rust thread
states currently lack explicit separation. Compiled layout and adjacent
synchronization state must be examined before claiming false sharing or
retaining a layout change.

### Completed Mac decoder lifecycle cohort

The frozen default-feature candidate passes all 16 comparisons across ten
scenarios, including ledger and no-ledger variants, with zero failures.
Three interleaved measured repetitions follow one warmup. The load monitor
made 459 checks without detecting compilation or heavy VM activity. Every
CPU and RSS delta is better than the reference. Percentages below are
`(ours / 7zz - 1) * 100`, so negative is better.

| Scenario | Wall delta | CPU delta | RSS delta |
| --- | ---: | ---: | ---: |
| Compressed stream Tall | +0.81% | -0.77% | -2.84% |
| Media Tall | -3.34% | -0.49% | -10.47% |
| Media T2 ledger | -1.22% | -1.06% | -6.93% |
| Media T2 no ledger | -1.13% | -1.01% | -4.68% |
| Media T4 ledger | -1.86% | -1.62% | -7.64% |
| Media T4 no ledger | -1.50% | -1.36% | -8.00% |
| Media T8 ledger | -0.29% | -0.50% | -6.58% |
| Media T8 no ledger | -0.28% | -0.52% | -8.87% |
| AES T2 ledger | -0.15% | -0.36% | -7.01% |
| AES T2 no ledger | -0.15% | -0.43% | -7.71% |
| AES T4 ledger | +0.13% | -0.33% | -7.54% |
| AES T4 no ledger | +0.48% | -0.14% | -11.54% |
| AES T8 ledger | -0.34% | -0.65% | -8.85% |
| AES T8 no ledger | -0.54% | -1.07% | -9.93% |
| Solid tree stream Tall | -1.00% | -0.68% | -1.93% |
| AES Tall | -0.91% | -0.80% | -10.71% |

Full-thread media peak RSS is 41.4 versus 46.3 MiB, compared with roughly
68–74 MiB in earlier candidates. The raw and report records are preserved in
sevenz's `mac-slot-handoff-sevenz.*.json.zst`; the source/binary fingerprint,
validation history and load-monitor record belong to the lifecycle bundle.
This closes these Mac decoder lanes, not the remaining encoder gaps or
unmeasured architectures.

### Completed x86 decoder lifecycle cohort

The monitored cohort completed all 16 comparisons with no failures and 734
compiler checks without interference. Fifteen comparisons meet all three
limits. Media T8 with the ledger measured CPU +3.16%, wall -0.96% and RSS
-4.01%; this borderline CPU result remains in the original evidence.

A separate five-repetition T8 confirmation used the same binary. The ledger
variant measured wall -3.15%, CPU +0.31% and RSS -3.67%; without the ledger,
the deltas were -3.08%, +0.31% and -4.28%. This larger cohort supports parity
under the hybrid host's scheduling variation; it does not erase the earlier
result. Both cohorts are retained as `x86-slot-handoff-sevenz.*.json.zst`
and `x86-slot-handoff-t8-five.*.json.zst` in sevenz's evidence directory.

### Tradeoff register

Exceptions are scoped to the measured workload and host. Proposed exceptions
remain outside acceptance until the operator approves them; a ratio close to
the limit does not silently become an exception.

| Status | Workload and evidence | Measured tradeoff | Reason and decision |
| --- | --- | --- | --- |
| Accepted by operator | Mac non-solid folder extraction, `mac-folder-widths` | T18 wall 0.243 versus 3.163 s (13.0x faster); CPU 3.757 versus 3.156 s (+19.0%); RSS 16.34 versus 5.81 MiB | The operator explicitly accepted about 20% more CPU for a 13x wall-time win. The larger RSS is also covered by the earlier allowance for a large timing win. Preserve parallel folder extraction. |
| Covered by earlier RSS allowance | Mac XZ delta-64 filter decode, `mac-current-lzma` | Wall -84.96%, CPU -85.24%, RSS +15.61% (roughly 1.5 MiB extra) | A large wall/CPU win with a small absolute memory cost. This is an earlier filter-path measurement, not a measurement of the final rebuilt binary. |
| Proposed; awaiting later approval | Other Mac XZ filter decode rows, `mac-current-lzma` | Wall 25.2–38.6% faster, CPU 27.1–38.4% lower, RSS 15.2–15.9% higher (roughly 1.5 MiB extra) | The whole measured process includes larger caller input/output buffers than xz's CLI. The product filter path wins substantially; changing only the probe would not be a product improvement. Retain the evidence and request a scoped decision later. |

The folder-width sweep also measured T1, T6, T8 and T12. T1 CPU is near
parity; the parallel widths trade increasing aggregate CPU for shorter wall
time. It is not necessary to reduce the operator-approved 13x speedup to
force the CPU ratio below 3%. The old x86 cycle profile excluded kernel
cycles; the width sweep records user and system CPU separately.

The quiet Mac encoder cohort (`mac-quiet-encode`) passes T4 and ordinary
full-thread L5 in all three metrics. T8 is +1.48% wall, +1.72% CPU and
+5.20% RSS. T16 is +3.66% wall/+3.27% CPU, with RSS +0.22%; full-thread AES
is +3.43%/+3.48%, with RSS +0.08%. These remain unresolved rather than
tradeoff exceptions. Both implementations sometimes retain an additional
compressed-output block; no source change has yet been justified for T8.

The shared-state layout experiment aligned `HashState`, `BtState` and
`LzState` to 128 bytes, matching the SDK's ownership-separation intent.
Compiled `MtShared` size grew from 1,768 to 2,048 bytes. Thirty native/SDK
checks passed, and all 60 whole-archive A/B outputs passed 7zz integrity;
unencrypted old/new archives were byte-identical. Nevertheless, Mac Tall
wall rose 2.63% against the previous binary and reached +4.24% against 7zz;
CPU was +3.58% against 7zz. T16 CPU remained +3.29% and AES Tall wall
+3.75%. The change was removed from the Mac candidate. Its T8 RSS benefit
does not justify retaining the timing regression. Evidence is retained as
`mac-cache-separation-ab.*.json.zst`, alongside source hashes and before/after
layout diagnostics. The x86 experiment was also rejected: T8 wall and CPU
rose 2.23% and 3.11% against the prior binary, while full-thread AES remained
+4.61% wall and +5.82% CPU against 7zz. Its 48 archive checks passed. The
source change was removed on both hosts; `x86-cache-separation-ab.*.json.zst`
preserves the measurements.

The match-transfer audit found large compiler-generated vectorization setup
around the pair-copy loops. A candidate checked each complete slice range
and copied the held BT result in one operation, in both the plain and
low-hash paths. It added no unsafe code and passed 31 SDK/parser tests.
Whole-archive measurements did not establish a useful improvement; Mac
full-thread AES wall remained +3.13% against 7zz. This experiment was also
removed. `mac-match-copy-ab.*.json.zst` retains its measurements.

### Non-solid encoder lifecycle audit

The audit covered folder sizing, coder configuration and reuse, output
buffering, completed-folder metadata, and final header encoding. The retained
changes follow the SDK's header settings (BT2, 273 fast bytes, a dictionary
capped at 1 MiB and reduced for the header size, one thread), reconfigure a
retained payload coder when only its properties change, store decoder
properties instead of full encoder options for completed folders, and use
4–256 KiB output buffers according to the known folder-size hint. A hint
never limits how many input bytes are accepted. Changed coder families still
get a fresh encoder; failed reconfiguration drops the unusable cached coder.

The metadata/buffering Mac comparison uses three measured repetitions after one warmup,
alternating candidate/reference order. All 60 generated archives passed
official 7zz integrity checks. Unencrypted before/after archives were
byte-identical. The comparison's baseline already has the SDK header
settings; those intentionally changed header compression from the earlier
candidate, without changing payload bytes. There were 333 load checks and
no detected compiler or heavy VM interference. The full workspace suite
passed 422 tests, including encrypted and BCJ2 archives; formatting and
all-target, all-feature Clippy passed.

| Mac non-solid encode | Wall delta vs 7zz | CPU delta | RSS delta | Candidate / 7zz RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Tree T1 | -0.18% | -0.15% | -2.68% | 20.45 / 21.02 |
| Tiny-file tree T1 | +0.93% | +0.76% | +0.34% | 9.25 / 9.22 |
| Encrypted tiny-file tree T1 | -1.45% | -4.32% | +6.57% | 11.16 / 10.47 |
| Tree T18 | -90.41% | +14.51% | +258.37% | 79.23 / 22.11 |
| Tiny-file tree T18 | -71.55% | +41.79% | +157.56% | 24.75 / 9.61 |

Tree T1 RSS fell 32.46% against the preceding candidate, from 30.28 MiB;
parallel tree RSS fell 48.89%, from 155.03 MiB, with wall 4.77% faster and
CPU 3.16% lower. The two unencrypted T1 cases meet all three thresholds.
Encrypted T1 still exceeds RSS by about 0.69 MiB and remains open. The two
parallel encoder cases are **proposed tradeoffs awaiting later approval**:
10.43x wall speedup with 14.51% more CPU for the tree, and 3.51x speedup
with 41.79% more CPU for tiny files. The accepted parallel *extraction*
exception does not automatically approve these encoder cases.

The intermediate coder-reuse-only x86 comparison reduced parallel-tree
wall by 18.16%, CPU by 20.37%, and RSS by 38.14% against its preceding
candidate. Against 7zz it was 8.12x faster with 55.11% more CPU and
4.03x RSS; that is also a proposed tradeoff, not threshold acceptance.
Its T1 tree RSS remained +5.29%. This intermediate cohort is retained
separately from measurements of the combined metadata/buffering candidate.

The combined x86 candidate passed 427 tests and doctests (one ignored
doctest), plus Clippy. Its 60 archive checks passed; 536 load checks found
no competing compiler or heavy VM workload. Deltas against 7zz were:

| x86 non-solid encode | Wall delta | CPU delta | RSS delta | Candidate / 7zz RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Tree T1 | -4.34% | -4.27% | +5.67% | 22.99 / 21.76 |
| Tiny-file tree T1 | -14.00% | -14.01% | +6.43% | 11.05 / 10.38 |
| Encrypted tiny-file tree T1 | -15.82% | -16.72% | +17.07% | 12.83 / 10.96 |
| Tree T16 | -87.60% | +53.53% | +280.67% | 82.32 / 21.62 |
| Tiny-file tree T16 | -81.07% | +10.33% | +57.56% | 22.55 / 14.31 |

The tree's combined changes reduce wall/CPU/RSS by 27.44%/29.03%/42.74%
against the preceding candidate at T16, and 4.39%/4.31%/14.48% at T1.
The T1 tree RSS gap remains open. The two tiny-file T1 rows and parallel
rows are recorded as **proposed tradeoffs awaiting later approval**:
they trade the listed extra RSS or aggregate CPU for shorter wall time.
They are not reported as meeting all three thresholds. Tiny-file T1 RSS
also rose 3.21% against the preceding candidate; that regression is retained
in the report rather than hidden by its timing advantage over 7zz.

An RSS measurement defect was found in the initial x86 comparison: a
Python launcher that had hashed large files in memory passed its inherited
RSS history into child measurements. A 256 MiB-parent reproduction reported
281,518,080 bytes for a trivial child; a small native fork/exec/wait4 launcher
reported 1,581,056 bytes instead. The partial affected cohort is retained
and marked invalid for RSS acceptance. Corrected measurements use the
native launcher and streaming file hashing. This change is to measurement
fidelity, not to the library or benchmarked product path.

Evidence: `mac-sdk-header-sevenz.*.json.zst`,
`mac-coder-reuse-ab.*.json.zst`, `mac-folder-metadata-ab.*.json.zst`, and
`x86-coder-reuse-child-ab.*.json.zst` and
`x86-folder-metadata-ab.*.json.zst` in sevenz's evidence directory;
source fingerprints, validation logs, heap diagnostics, driver source and
the RSS reproduction are retained in the lifecycle validation bundle.

The subsequent ownership pass moves serial entries instead of cloning them,
reserves the known entry count, shares equivalent decoder properties across
fresh configurations, and avoids cloning an already-owned AES configuration
just to refresh its IV. Single-stream sizes and CRCs use inline storage.
Its Mac workspace suite passed all 422 tests and Clippy. All 60 archive
checks passed, unencrypted before/after bytes matched, and 332 load checks
found no compiler or heavy VM interference.

| Mac ownership pass | Wall delta vs 7zz | CPU delta | RSS delta | Candidate / 7zz RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Tree T1 | -0.69% | -0.79% | -3.48% | 20.39 / 21.12 |
| Tiny-file tree T1 | -0.01% | +0.75% | -6.76% | 8.62 / 9.25 |
| Encrypted tiny-file tree T1 | -1.84% | -4.15% | +4.33% | 10.92 / 10.47 |
| Tree T18 | -90.44% | +12.95% | +245.65% | 76.91 / 22.25 |
| Tiny-file tree T18 | -71.53% | +37.16% | +162.87% | 24.89 / 9.47 |

Against the preceding metadata/buffering candidate, tiny-file T1 wall/CPU/RSS
fell 2.34%/2.38%/7.23%. Encrypted T1 RSS fell another 1.96%, but its 4.33%
gap remains open. Parallel encoder rows remain proposed tradeoffs awaiting
approval. Evidence is `mac-folder-ownership-ab.*.json.zst`.

The x86 ownership pass also passed the full workspace suite and Clippy.
All 60 archive checks passed, unencrypted before/after bytes matched, and
529 load checks found no compiler or heavy VM interference. Tree T1 now
meets all three thresholds. The tiny-file and parallel cases remain
proposed tradeoffs, not unconditional acceptance.

| x86 ownership pass | Wall delta vs 7zz | CPU delta | RSS delta | Candidate / 7zz RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Tree T1 | -5.11% | -5.16% | +1.75% | 22.21 / 21.83 |
| Tiny-file tree T1 | -14.04% | -14.08% | +4.17% | 10.84 / 10.41 |
| Encrypted tiny-file tree T1 | -18.91% | -19.38% | +13.84% | 12.53 / 11.01 |
| Tree T16 | -87.68% | +54.88% | +285.55% | 84.61 / 21.95 |
| Tiny-file tree T16 | -80.28% | +13.47% | +57.98% | 22.57 / 14.29 |

Tree T1 RSS fell 3.85% against the metadata/buffering baseline, with wall
and CPU within 0.15% of it. Tiny-file T1 RSS fell 5.84%. Parallel tree RSS
rose 4.56% in this cohort; the recorded tradeoff includes that result.
Evidence is `x86-folder-ownership-ab.*.json.zst`.

A further serialization candidate stores the completed folder descriptor
as bytes and writes sub-stream CRCs directly. It passed all 422 Mac tests
and Clippy, as well as the x86 full suite and Clippy. Each host completed
60 archive checks with byte-identical unencrypted before/after archives.
The compiler/heavy-VM monitors recorded 332 Mac and 525 x86 checks without
flagged activity. The three-repetition medians against 7zz were:

| Serialization candidate | Wall delta | CPU delta | RSS delta | Candidate / 7zz RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Mac tree T1 | -0.53% | -0.48% | -4.15% | 20.22 / 21.09 |
| Mac tiny-file tree T1 | -2.55% | -2.38% | -8.35% | 8.58 / 9.36 |
| Mac encrypted tiny-file tree T1 | -0.87% | -3.96% | +3.58% | 10.86 / 10.48 |
| Mac tree T18 | -90.48% | +13.79% | +264.69% | 80.86 / 22.17 |
| Mac tiny-file tree T18 | -71.43% | +47.33% | +164.98% | 25.30 / 9.55 |
| x86 tree T1 | -5.12% | -5.14% | +1.27% | 22.13 / 21.86 |
| x86 tiny-file tree T1 | -15.28% | -15.29% | +4.82% | 10.87 / 10.37 |
| x86 encrypted tiny-file tree T1 | -17.56% | -17.87% | +13.88% | 12.21 / 10.72 |
| x86 tree T16 | -87.36% | +57.02% | +271.82% | 81.48 / 21.91 |
| x86 tiny-file tree T16 | -80.18% | +13.21% | +59.37% | 22.65 / 14.21 |

Encrypted T1 RSS fell 2.25% on Mac and 2.34% on x86 against the ownership
baseline. Its remaining excess is not accepted as parity. Mac parallel tree
RSS rose 7.57% in this cohort; parallel timing and memory tradeoffs remain
explicitly proposed. Evidence is `mac-folder-serialization-ab.*.json.zst`
and `x86-folder-serialization-ab.*.json.zst`.

The remaining T1 non-solid decode RSS audit traced header parsing, stream
maps, folder scheduling, coder construction and teardown, and the benchmark
consumer. Heap snapshots of the real tree decode showed 16,618 live
allocations / 3,705,120 allocated bytes versus 1,228 / 2,787,584 for 7zz.
These are diagnostics, not timed RSS results. Per-folder coder vectors and
per-entry names dominate allocation counts; both are public metadata types.
T1 also materializes an unnecessary folder scheduling vector. The raw
LZMA2 adapter input buffer is 64 KiB, and the encoded-header decoder is
dropped before the actual header is parsed. Neither a retained header
decoder nor a supposed 1 MiB adapter input buffer explains the gap.

The next grouped candidate removes unused solid/BCJ2 metadata from ordinary
encoder folders, stores parsed coder IDs and properties more compactly
(48 rather than 64 bytes per coder on ARM64), and walks sequential folders
without allocating a scheduling vector. Public metadata types and complete
long coder properties are preserved. Mac formatting and Clippy passed;
all 422 tests passed, but Nextest flagged an output-handle leak in
`a_narrowed_decode_reads_ahead_for_the_threads_it_will_run`. That warning
is being investigated separately and is not declared clean validation.
One hundred focused repetitions passed without reproducing the warning;
this does not resolve the warning observed during the full suite. The
reader's worker-pool shutdown joins all worker handles, and no subprocess
is created in the flagged test's path. No timeout or assertion was weakened.
The Mac compact-folder timing cohort was interrupted after competing browser
activity was observed and full-thread samples slowed; its partial results
are diagnostic only. The operator moved subsequent measurements to x86.
The x86 full suite passed 427 tests with no failures and one ignored doctest;
Clippy also passed. The completed encoder cohort contains 60 successful
official integrity checks, byte-identical unencrypted before/after archives,
and 525 load checks with no detected compiler or heavy-VM interference.

| x86 compact-folder encode | Wall delta | CPU delta | RSS delta | Candidate / 7zz RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Tree T1 | -4.82% | -4.81% | -6.73% | 20.29 / 21.76 |
| Tiny-file tree T1 | -14.05% | -14.21% | +0.07% | 10.52 / 10.51 |
| Tiny-file tree T1 AES | -15.03% | -15.42% | +15.55% | 12.39 / 10.73 |
| Tree T16 | -87.15% | +54.94% | +277.88% | 81.76 / 21.64 |
| Tiny-file tree T16 | -78.87% | +23.04% | +60.50% | 22.93 / 14.29 |

Tree T1 RSS fell 7.84% against the preceding binary with CPU +0.36%.
Unencrypted tiny-file T1 passes this cohort, but both binaries' RSS varied
below the preceding cohort; its closure needs confirmation rather than
attributing that whole reduction to this patch. Encrypted tiny files remain
open. Tree T16 wall rose 4.44% against the preceding binary while CPU fell
0.21%; this is an unresolved baseline regression requiring confirmation,
not an approved tradeoff. The parallel rows' CPU/RSS tradeoffs against 7zz
remain proposed only. Raw samples are in sevenz's
`x86-folder-compact-ab.{raw,report}.json.zst`.

A separate five-repeat confirmation did not reproduce the T16 regression:
wall was -2.83%, CPU -0.06%, and RSS +0.27% against the preceding binary.
Unencrypted tiny-file T1 again passed against 7zz (-14.37% wall/CPU,
+0.79% RSS). Both cohorts are retained; the first T16 result is not removed.
The confirmation's monitor started after the first samples and made 240
checks without detecting a compiler. Its partial coverage does not establish
an idle host throughout. Evidence is
`x86-folder-compact-confirm-ab.{raw,report}.json.zst`.

The decoder cohort completed after the signed checkpoints. All five cases
preserved decoded bytes, entry counts and digests between binaries, with
verification enabled and AWS-LC selected. Its 545 load checks detected no
compiler or heavy-VM interference. None of the five cases regressed timing
by more than 3% against the preceding binary, but compact representation
did not produce a lower measured process RSS on this host:

| x86 compact-folder decode | Wall delta | CPU delta | RSS delta | Candidate / 7zz RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Non-solid tree T1 | -0.13% | -0.13% | +24.00% | 9.61 / 7.75 |
| Non-solid tree T1 streaming | -0.32% | -0.29% | +24.52% | 9.64 / 7.74 |
| Non-solid tree T16 | -88.75% | +51.15% | +110.52% | 16.26 / 7.72 |
| Tiny encrypted tree T1 | -20.68% | -22.42% | +15.11% | 7.68 / 6.67 |
| BCJ2 T1 | +1.02% | -16.58% | -7.21% | 40.65 / 43.81 |

The T1 tree RSS gap therefore remains open on x86 as well as Mac. The
parallel x86 and encrypted-tree timing/RSS trades are recorded for later
operator review, not marked approved. Samples are in sevenz's
`x86-folder-compact-decode-ab.{raw,report}.json.zst`.

Linux residency sampling, kept separate from acceptance timings, explains
why smaller structures did not yield a consistent process-RSS reduction.
Across three launches each, the preceding binary used 4.105 MiB anonymous
memory at the sampled peak and the compact candidate used 3.730 MiB: a
384 KiB reduction. File-backed residency varied enough to obscure that
change in the short-process RSS measurements. Official 7zz used about
1.8 MiB anonymous memory, leaving roughly 1.9 MiB to investigate.

The subsequent graph audit covers header parsing and graph validation,
stream maps and pack ranges, coder iteration, memory admission, multi-stream
decoder construction, and pipeline offloading. The new candidate stores
only the output size for one-input, one-output folders with no edges and
packed stream zero; every other graph retains its full tables. Public coder
and file-entry fields, full-width stream values, and hostile-header checks
remain intact. A regression test covers default metadata, full-width output
sizes and clone independence. Formatting and Clippy passed locally; 115
library-source/build-script hashes match the x86 checkout. The x86 full
suite passed 428 tests with no failures and one ignored doctest; all-feature
Clippy and the default AWS-LC release build also passed.

Three measured repetitions after a warmup compared the checkpoint, graph
candidate, and official 7zz. All five candidate/checkpoint comparisons stayed
within 3% for wall, CPU and RSS. Digest, byte-count and entry-count comparisons
passed; 541 load checks found no compiler or heavy VM interference.

| x86 graph decode | Wall delta vs checkpoint | CPU delta vs checkpoint | RSS delta vs checkpoint | Wall / CPU / RSS delta vs 7zz |
| --- | ---: | ---: | ---: | --- |
| Non-solid tree T1 | -1.48% | -1.49% | -0.72% | -1.09% / -1.06% / +18.92% |
| Non-solid tree T16 | -2.29% | -0.54% | -2.21% | -88.90% / +52.86% / +98.34% |
| Non-solid tree T1 streaming | +0.30% | +0.31% | -3.12% | -0.11% / -0.12% / +16.66% |
| Encrypted tiny files T1 | +1.04% | +0.97% | +0.72% | -18.31% / -19.81% / +15.82% |
| BCJ2 T1 | +0.57% | +0.55% | +1.06% | +13.77% / -6.03% / -7.66% |

Evidence: sevenz `x86-folder-graph-decode-ab.{raw,report}.json.zst`.
Tree T1 RSS remains open at 9.13 versus 7.68 MiB. Parallel tree CPU/RSS
remain a proposed tradeoff, not an approved exception. The BCJ2 reference
wall miss occurs in both checkpoint and candidate binaries; 7zz used
0.746 CPU seconds in 0.616 wall seconds, while the candidate used 0.701
for both. The preceding cohort's +1.02% wall result does not resolve this miss.

The pinned SDK source explains a relevant contract difference:
`CHandler::InitCommon` enables `_useMultiThreadMixer` independently of the
coder thread count, and `CDecoder::Decode` selects `CMixerMT` from that flag.
The `mtf` property disables that pipeline separately. Our `offload_plan`
returns no auxiliary stages at T1, preserving the one-thread contract.
Five measured repetitions after a warmup compared both reference modes:

| BCJ2 placement | Product T1 wall / CPU seconds | 7zz T1 wall / CPU seconds | 7zz T1, mixer off wall / CPU seconds |
| --- | --- | --- | --- |
| Unpinned | 0.702 / 0.702 | 0.595 / 0.720 | 0.696 / 0.696 |
| Same P core | 0.676 / 0.676 | 0.682 / 0.682 | 0.689 / 0.688 |
| Same E core | 0.973 / 0.973 | 0.971 / 0.971 | 0.980 / 0.980 |

Product RSS was 40.55–40.69 MiB, versus 43.50–44.40 MiB for the reference
modes. These controls isolate the pipeline overlap: they do not replace the
library's T1 contract or authorize adding threads behind a T1 request.
The operator reconfirmed this already-known distinction: use our T2 for
SDK T1 pipeline parity and stop treating literal T1/T1 as a kernel gap.
This is now documented beside `offload_plan`, in `docs/benchmarking.md`,
and in the harness README. The harness already preserves a separate
strictly serial BCJ2 parity reference with `-mmtf=off`.
Evidence: sevenz `x86-bcj2-thread-audit.*.json.zst`.

The graph candidate now also stores only the two output sizes for ordinary
two-coder chains, with distinct representations for both edge directions.
Full tables remain for other graphs. A differential metadata test checks
both orders, empty initialization, full-width sizes and independent clones.
All three focused storage tests passed locally. The full x86 suite passed
429 tests with no failures and one ignored doctest; all-feature Clippy and
the default AWS-LC build passed. Three measured repetitions after a warmup
completed, with matching digests and 838 load checks finding no compiler or
heavy VM interference.

| x86 two-coder graph decode | Wall / CPU / RSS delta vs single-graph candidate | Wall / CPU / RSS delta vs 7zz |
| --- | --- | --- |
| Non-solid tree T1 | -0.43% / -0.44% / +3.25% | -0.54% / -0.54% / +18.01% |
| Non-solid tree T16 | -0.13% / -0.56% / +2.17% | -88.52% / +54.54% / +100.56% |
| Non-solid tree T1 streaming | +1.03% / +1.01% / +3.88% | +0.05% / +0.02% / +18.84% |
| Encrypted tiny files T1 | +0.67% / +0.65% / -2.95% | -15.36% / -17.23% / +14.44% |
| BCJ2 product T2 / reference T1 | -0.28% / -0.24% / +0.52% | +9.56% / -4.04% / +104.57% |
| AES+COPY T1 | +1.68% / +1.66% / 0.00% | -50.34% / -45.59% / -21.97% |
| ARM64 branch filter T1 | -0.59% / -0.59% / +0.79% | -2.38% / -5.80% / -3.64% |
| Delta filter T1 | +6.16% / +6.17% / +0.63% | +6.16% / +3.32% / -4.64% |

Evidence: sevenz `x86-folder-graph-pair-decode-ab.*.json.zst`. This cohort
does not establish acceptance: the short tree RSS rows and delta timing
exceed the candidate regression gate. Delta candidate wall samples range
from 4.925 to 5.621 seconds, requiring a core-placement control before
attributing that spread to the metadata change. The agreed T2 BCJ2 pairing
also exposes a real adaptive-reader memory/timing gap in both binaries;
that is separate from the settled literal T1 contract comparison.

Five-repetition delta controls pinned every contender to the same core,
after a warmup. On the P core, the candidate changed wall/CPU/RSS by
+1.39%/+1.64%/+0.82% versus the preceding binary, and
-4.41%/-4.41%/-4.07% versus 7zz. On the E core the corresponding deltas
were -0.26%/-0.26%/+0.69% and -4.09%/-4.09%/-4.32%. All controls passed
byte/digest checks; 371 P-core and 497 E-core load samples recorded no
compiler interference. Both pinned controls meet the gates. The original
unpinned result remains evidence rather than being discarded.

Separate sampled RSS diagnostics are not acceptance timing. Across three
non-solid T1 runs, both product binaries used exactly 3.23 MiB anonymous
memory at their sampled RSS peaks; 7zz used 1.80 MiB. Candidate median RSS
was 9.08 MiB versus 8.59 MiB before, with the difference in file-backed
residency. Executable text grew only 2,124 bytes. This does not establish
an additional metadata allocation or close the remaining heap/RSS gap.
Evidence: sevenz `x86-folder-graph-pair-{pcore,ecore}-ab.*.json.zst` and the
residency report retained in `decoder-lifecycle-validation.json.zst`.

The subsequent decoder audit traced scanner completion, admission, dispatch,
the caller-thread fallback and output delivery. A complete stream with one
run still allocated that run's full output for a worker even when the
dictionary was smaller and no second run could overlap. The current
candidate selects the existing streaming fallback only after the end marker
proves the stream has one run, and only when its dictionary is smaller than
the complete output. Short runs and genuine multi-run inputs retain worker
dispatch. The focused Nextest sweep passed all 68 adaptive unit and
integration tests, including compressed data, split CRCs, partial drains
and a malformed range coder. The new path retains the worker's located
`CorruptRun` error. The sevenz reader regression passed for one and two runs
at two and eight threads, including repeated EOF reads. All-target and
all-feature crate Clippy passed again after the error-location adjustment.
The x86 suite
passed with required pinned C SDK oracles: lzma reported 400 passed, zero
failed and three ignored across 30 targets; sevenz all-features reported
427 passed, zero failed and one ignored across 18 targets; sevenz default
AWS-LC reported 404 passed and zero failed across 18 targets. Formatting,
all-target/all-feature Clippy, the lzma no-std check and the AWS-LC release
benchmark build also passed. The ignored native guard-page tests and
doctests were not separately run; the SDK assembly oracle was not required.
These gates cover the lone-run and AES-facts source bundle, before the
subsequent owned-push and positional API integration. The completed encoder
matrix uses the preceding source and is kept separate.

The completed lone-run/AES-facts comparison contains 96 process samples
(three measured repetitions after a warmup for each contender), 16 byte/digest
checks and 1,960 load checks with no compiler interference. Median deltas
against 7zz are:

| Decode case | Wall delta | CPU delta | RSS delta |
| --- | ---: | ---: | ---: |
| BCJ2 T2 | +9.85% | -2.01% | +21.52% |
| BCJ2 T2 streaming | +12.61% | +0.21% | +21.87% |
| Single-run T16 | +1.18% | +1.18% | +0.58% |
| Multirun T2 | -1.95% | -2.48% | +0.02% |
| Multirun T16 | +2.42% | +0.55% | +0.02% |
| Near-incompressible L1 T16 | +0.51% | +0.40% | -5.83% |
| Encrypted L1 T16 | -1.74% | -2.02% | -8.43% |
| AES stored T2 | -51.46% | -46.48% | -22.17% |

The BCJ2 reference uses the agreed default SDK T1 mixer. Its product RSS
fell from about 89.5 MiB to 53.5 MiB, a 40% reduction, but neither BCJ2 row
is closed. Streaming CPU increased 3.77% against the preceding product
binary, while remaining within 0.22% of SDK. A separate trace records no
run workers, one 56 MiB output run, and 21.75 MiB of retained packed input.
The pipeline still assigns its auxiliary thread to a smaller side coder
under the assumption that the main LZMA2 coder is parallel. A main-stage
assignment experiment was measured separately and rejected below.
Evidence: sevenz `x86-lone-run-decode-ab.*.json.zst` and retained load logs.

Assigning BCJ2's main LZMA2 coder a pipeline stage passed all 12 pipeline
tests and improved the original T2 wall time by 10.55% versus the product
baseline, to 0.18% faster than SDK. However, CPU was 4.62% above SDK and
RSS remained 20.34% above it. Streaming CPU was 3.20% above SDK. A separate
control was encoded with a 1 MiB dictionary and verified to contain 15 runs:
its T2 wall/CPU regressed 6.07%/6.21% against the product baseline, while
T4 CPU/RSS exceeded SDK by 8.70%/10.54%. T16 control CPU passed but RSS
was 3.51% above SDK. All 195 load samples were clear. The experiment was
removed from both source trees, and the original binary restored; the full
seven-case cohort and source diff remain as
`x86-bcj2-pipeline-ab.*.json.zst` and lifecycle diagnostics. Static main-stage
assignment is not a landed fix or an approved tradeoff.

The follow-up source audit compared `Lzma2Dec_Decode_ST`, `CMixerMT::Init`,
`CStreamBinder::Read`/`Write`, the BCJ2 refill path, and the product's scanner,
input retention and pipeline ownership. Three sampled mapping diagnostics
attribute the remaining lone-run gap to anonymous input residency beside the
ring dictionary: the product retained about 48 MiB anonymous memory versus
39 MiB for SDK, while its heap was only about 1.5 MiB. This does not support
a global allocator-retention workaround.

A bounded seek probe now uses the existing LZMA2 scanner, skips payload, and
accepts only exact single-run framing and declared bounds. It declines on a
second run, malformed input, I/O failure or its work bound. Adaptive readers
are excluded so download chase cannot probe future coverage. The resulting
serial ring may occupy a pipeline stage without allocating a complete packed
run. Four probe tests, a thread-budget test, and a product-path regression
cover the selection and adaptive exclusion. The pre-ring combined x86 suite
passed 441 tests with one ignored doctest; the added product-path regression
and all-feature Clippy also passed.

The nine-case probe cohort retained 108 process samples, 18 matching digests
and 245 clear load samples. Product T2 versus SDK T1 measured wall -1.31%,
CPU +2.32%, RSS -2.24%; streaming CPU remained +3.10%. T4/T8/T16 retained
CPU gaps of +5.98–7.40% and RSS gaps of +5.21–5.92%. The 15-run controls
kept their parallel path; some absolute reference gates still failed despite
small candidate changes. Evidence: sevenz `x86-bcj2-seek-ab.*.json.zst`.

The next pipeline candidate retains initialized recycled pieces, avoiding a
full zero-fill at every refill, and queues 512 KiB per edge instead of 1 MiB,
matching the amount negotiated between SDK mixer coders. All 14 pipeline
tests passed. In its 13-case cohort, both BCJ2 T2 forms pass all three SDK
gates: ordinary -2.41%/+1.86%/-4.38% and streaming
-2.07%/+1.28%/-4.02% (wall/CPU/RSS). Wider single-run BCJ2 still fails CPU,
and T8 also fails wall. Multi-run T16 still fails wall. The ordinary T16
control changes wall by +3.54% against the preceding product despite being
within +2.65% of SDK; the untouched path's spread is retained, not discarded.
All 26 digest checks pass and 602 load samples show no compiler interference.
Evidence: sevenz `x86-bcj2-pipe-ab.*.json.zst`.

Source inspection also found a graph-wide wakeup on every pipe update, while
the SDK binders signal individual edges. A per-pipe notification candidate
passed the combined 450-test sevenz all-feature suite (one ignored doctest)
and Clippy, but failed performance acceptance and was removed. The 13-case
cohort retained 156 process samples, 26 matching digests and 611 load samples
without compiler interference. Single-run T16 passed the SDK gates at
-2.82%/+2.76%/+2.94% (wall/CPU/RSS), while multi-run T16 regressed
+13.65%/+29.48%/-3.47% against the preceding product. T4/T8 single-run CPU
was still +5.00%/+5.87% above SDK. The unchanged ARM64-filter control also
varied by -17% in time, so this cohort does not establish a causal scheduler
explanation. Evidence and the rejected patch remain in
`x86-bcj2-wake-ab.*.json.zst` and the lifecycle bundle.

The next source audit found the BCJ2 reader refilling every empty input,
where the SDK's `CBaseDecoder::ReadInStream` uses the decoder's requested
stream. The candidate exposes that existing state and refills only the
required input. Interrupted reads are retried without losing output already
consumed during the call; unused side coders retain their initialization
and error checks at completion. Regression tests cover unrelated-input
blocking, interrupted partial-word reads and unused-side errors. Validation
and measurement are pending; no speed claim is made yet.

### Current x86 encoder confirmation

The graph-single binary was compared with 7zz at L5 T4, T8, T16 and T16 AES,
with three measured repetitions after a warmup and alternating order. All
32 archives passed official integrity checks; unencrypted product archives
were byte-identical across repetitions. The 1,041 load checks found no
compiler interference. No new encoder change was included in this cohort.

| Setting | Wall delta | CPU delta | RSS delta |
| --- | ---: | ---: | ---: |
| T4 | -7.61% | -2.97% | +10.47% |
| T8 | -4.66% | +0.67% | +3.75% |
| T16 | +3.06% | +2.91% | -0.49% |
| T16 AES | +3.45% | +3.40% | -0.54% |

These misses remain open, including the intermediate-width RSS rows. The
T4 RSS difference is approximately one compressed-output block, but that
observation is not yet a lifecycle attribution. SDK block-slot counts,
in-order delivery and ownership return must remain intact when investigating
it. Evidence: sevenz `x86-current-encoder-reference.*.json.zst`.

Twelve subsequent encoder runs sampled `/proc` mappings and verified every
generated archive with 7zz. They are allocation diagnostics, not acceptance
timings. Product T4 sampled peaks were 328.65, 300.24 and 300.34 MiB;
reference peaks were 296.78–296.86 MiB. Product T8 ranged from 610.15 to
645.92 MiB, while the reference used 643.69–643.89 MiB. The high product T4
sample had three large output-like anonymous mappings, versus two in its
lower samples and the reference. This supports variable output occupancy
as the next attribution target; it does not yet identify which lifecycle
transition caused it. Huge pages were lower in the product: 224 versus
232 MiB at T4, and 446 versus 464 MiB at T8. Removing huge-page advice is
not supported by these results. The sampled runs do not replace the
earlier timed RSS misses. Full mapping totals, largest mappings, sampled
series, binary/fixture hashes and integrity outcomes are retained in
`decoder-lifecycle-validation.json.zst`.

The library encoder matrix completed all 17 scenarios without failures,
each with three measured repetitions after a warmup. These use the source
preceding the lone-run decoder candidate. The process monitor recorded 622
samples with no compiler interference; it began during the first scenario,
so the initial launch is not covered. The following RSS tradeoffs are
proposed for operator review, not approved exceptions:

| Single-thread XZ encode | Wall delta vs xz | CPU delta | Product / xz RSS | RSS delta |
| --- | ---: | ---: | ---: | ---: |
| Preset 1 | -38.74% | -38.43% | 17.81 / 10.53 MiB | +69.14% |
| Preset 5 | -64.71% | -64.56% | 118.77 / 95.02 MiB | +24.99% |
| Preset 9 | -65.26% | -65.16% | 774.39 / 674.73 MiB | +14.77% |

The four multi-threaded preset-5 rows meet all three thresholds against xz:
wall improves 45.67–63.91%, CPU improves 48.16–63.96%, and RSS ranges from
7.37% lower to 0.52% higher. These results do not close the separate sevenz
T4/T8 RSS gaps against 7zz.

The eight branch-filter encode rows improve wall by 23.90–32.15% and CPU
by 22.12–30.62%, with 16.4–16.9 MiB RSS against xz's 10.5–10.6 MiB
(55.30–60.69% higher). Delta4 and delta64 improve wall by 43.04% and
43.46%, CPU by 42.44% and 42.90%, with 17.3 and 18.0 MiB RSS against
10.5 MiB (65.55% and 70.36% higher). These are also proposed RSS tradeoffs,
not accepted exceptions. Compressed size ratios round to 1.000 in the
harness, but the exact byte counts differ; this is not bit-identical xz
output. Evidence: `x86-current-lzma-encoder-matrix.*.json.zst`.

### AWS-LC bulk AES audit

The production default already uses bulk, unpadded AES-256-CBC. The audit
traced the encoder and decoder, their streaming bridges, key derivation,
`aws-lc-rs` 1.18.1 and its locked `aws-lc-sys` 0.45.0 implementation:

- `AwsLcAes256Cbc` and `AwsLcAes256CbcEnc` retain their expanded key across
  calls. Each call reaches `AES_cbc_encrypt` with the whole buffer. The
  hardware dispatcher selects `aes_hw_cbc_encrypt` on the measured ARM64
  and x86 hosts, including the multi-block decryption kernel.
- The encryption writer coalesces ordinary writes into 64 KiB calls. Final
  zero padding and flush boundaries preserve the 7z format. The decoder
  decrypts directly in large caller buffers and uses a lazy 64 KiB buffer
  for small callers. Short underlying reads can still yield small batches;
  forcing read-to-fill would change streaming latency and error behavior.
- The EVP streaming CBC interface selects the same hardware function on
  these hosts. Its public Rust interface supplies PKCS7 padding, whereas
  7z needs unpadded CBC with a declared output size. It offers no additional
  hardware kernel here and is not a justified replacement.
- KDF SHA-256 updates are already batched and password clones share the
  bounded derived-key cache. Fresh per-folder IVs remain independent of
  that cache. Independent folders can encrypt concurrently; blocks within
  one CBC encryption stream depend on the previous ciphertext.

The diagnostic processed 256 MiB per process, with three measured runs
after one warmup and alternating case order. These are kernel measurements,
not archive acceptance results. Wall, user-plus-system CPU and peak RSS
are all retained in sevenz's `aes-bulk-audit.json.zst`, along with the probe,
driver and source fingerprints.

| Host and operation | 4 KiB GiB/s | 64 KiB GiB/s | 256 KiB GiB/s | 1 MiB GiB/s |
| --- | ---: | ---: | ---: | ---: |
| Mac encrypt in place | 2.223 | 2.221 | 2.236 | 2.170 |
| Mac copy then encrypt | 2.145 | 2.167 | 2.153 | 2.138 |
| Mac decrypt in place | 14.721 | 15.349 | 15.564 | 15.560 |
| x86 encrypt in place | 1.408 | 1.414 | 1.411 | 1.416 |
| x86 copy then encrypt | 1.316 | 1.344 | 1.299 | 1.330 |
| x86 decrypt in place | 7.209 | 7.428 | 7.074 | 8.052 |

Keeping 64 KiB is supported by these results. The Mac copy-then-encrypt
process peaked at 17.78 MiB with that batch and 18.72 MiB with 1 MiB batches,
without a throughput gain. At 64 KiB the copy adds about 2.8 ms of measured
CPU per 256 MiB on Mac and 11.8 ms on x86. Removing it would require an
owned or mutable output-buffer route through the current `Write` bridge;
its modest benefit does not justify a speculative pipeline rewrite.

A real x86 full-thread encrypted archive was also profiled and passed
official 7zz integrity verification. AWS-LC CBC accounted for 0.31% of
sampled P-core user cycles and was below the 0.15% report cutoff on E cores.
Tree walking, match discovery and parsing dominate both core classes.
The two hardware event groups are not added as unweighted percentages,
and profiling results are not substituted for wall/CPU acceptance.

An optional RustCrypto bulk-CBC experiment was rejected. It passed 422
Mac tests and expanded cross-backend differential tests, and compressed
archive measurements remained within 3% of 7zz. However, real stored and
encrypted archives were 42–45% slower than the existing loop on Mac. The
x86 experiment also showed no useful gain (streaming wall/CPU +3.36%/+3.33%
against the existing loop). Both production changes were removed. The
expanded correctness tests and `mac-native-aes-ab`,
`mac-native-aes-store-ab`, and `x86-native-aes-store-ab` evidence remain.
The operator clarified that RustCrypto needs correctness, not performance
work; subsequent performance work targets AWS-LC and the default path.
No AES production change is retained from this audit.

Full-thread L5 timing and encoder RSS gaps remain open; no fleet-wide
acceptance is claimed. At the operator's request, signed checkpoints were
created and verified: lzma `4622fd0` and sevenz `b8fde3c`. Subsequent graph
metadata work and measurements continue after those checkpoints. Nothing
was pushed or released; no dependency backend or fixture bytes were added.

### Chase consumer integration

The separate AES+COPY integration adds validated public coder facts,
password-owned bounded key caching and zeroizing derived keys. Sevenz's
signed `671f39c` was integrated as `fd45a0a`. Weaver's signed `74ee5f3`
routes eligible stored encrypted members through its existing AWS-LC
direct-store path. Its final 15 focused tests and affected-package Clippy
passed. A real demotion bug found by the tests was fixed: a late plaintext
CRC failure now preserves all already-posted ciphertext under the existing
article CRC checks, including earlier volumes in the same drain. Successful
drains still release staging. Temporary local dependency resolution is
excluded from the Weaver commit; integration into its release branch still
requires an operator-owned published dependency decision. No throughput
claim is made from these correctness tests.

Sevenz's signed `6fd3865` and compatibility correction `798d605` were applied
to the local performance tree, preserving its graph metadata changes.
`PositionalBlockDecoder` shares parsed metadata, password state and a
positional source, while each executing task owns its cursor. COPY and AES
can deliver from bounded owned buffers through the existing direct entry
API. Eight focused tests passed in the combined tree, as did formatting
and all-target/all-feature Clippy. The agent's broader focused selection
also passed in both crypto feature configurations.

COPY's old zero-allocation admission remains intact for explicit finite
memory limits: those callers retain the 64 KiB caller-buffer fallback.
The default unlimited library configuration can use the new bounded push
buffer. Weaver supplies finite extraction budgets, so its COPY throughput
benefit remains open pending a shared output reservation model. AES bulk
reads into COPY's output buffer do not allocate a second plaintext buffer.
The combined push/positional x86 tree passed 435 all-feature tests (one
ignored doctest) and 412 default AWS-LC tests across 19 targets, plus Clippy
and its release benchmark build. A subsequent constructor guard refuses
interspersed no-stream entries that the existing block walk cannot represent;
it fails before accessing the source. The newer run API maps those members
explicitly instead of inheriting that block-walk restriction.

The signed ring primitives (`68d3504`) and sevenz positional adapter
(`dc66888`) are now integrated locally. They add explicit caller-owned jobs,
bounded input and ring storage, partial positional writes, cancellation and
checksum segments. Ordinary ordered reader scheduling is unchanged. The
agent's focused native tests, pinned C SDK differential test, Clippy and
no-std checks passed. The combined x86 lzma and required pinned C SDK suite
passed 395 tests across 26 targets, with zero failures and three ignored
tests; Clippy and both no-default-feature builds (with and without std)
passed. The SDK assembly oracle was not required. The combined sevenz
all-feature suite passed 450 tests, zero failures and one ignored doctest.
Parent review's fallible allocation and final-cancellation corrections
(`52ee918`) are included. Weaver's
guarded partial files, repair epochs and replay integration remain in
progress.

### Positional ring comparison and internal use

The integrated public run API was measured on the x86 host against the
ordered reader, with real file output and `sync_data` in both arms. Eight
cases have one warmup and three measured repetitions, alternated by arm.
All 64 output files matched official 7zz output by SHA-256 and length;
both paths verified archive CRCs. All 2,755 host-load samples were free of
competing compiler processes. These compare library paths, not a new
all-architecture parity claim.

| Case | Wall change | CPU change | RSS change |
|---|---:|---:|---:|
| compressed multi-run T2 | -2.78% | -0.28% | -86.08% |
| compressed multi-run T4 | -1.85% | -0.17% | -86.43% |
| compressed multi-run T8 | -12.70% | -2.39% | -86.58% |
| compressed multi-run T16 | -8.61% | -2.10% | -86.58% |
| single-run T2 | +2.28% | -0.56% | -70.38% |
| single-run T16 | -1.58% | -0.89% | -70.50% |
| near-incompressible T2 | -0.58% | +0.84% | -43.58% |
| near-incompressible T16 | -9.99% | +3.63% | -73.21% |

The final row is an **operator-approved tradeoff** (2026-10-13): the
3.63% CPU increase is accepted for 9.99% shorter wall time and 73.21% lower
RSS in this measured x86 T16 case. This approval does not extend to other
workloads or architectures. Raw samples and
medians are retained in sevenz's `x86-positional-ring-ab.{raw,report}.json.zst`.
At T16 the compressed fixture used 260 MiB rather than 1,937 MiB.

Internal audit covered adaptive dispatch, worker storage, ordered delivery,
checksumming, positional source ownership and confined filesystem creation.
Ordered workers currently use their eventual output buffer as dictionary;
replacing each with a small ring queue would block later workers behind the
first run and can serialize decoding. Positional output removes that
ordering constraint and also avoids retaining whole compressed runs.

The internal extraction candidate adds `ArchiveReader::extract_to`, using
the existing confined creation, symlink rejection, hard-link-safe replacement
and timestamp helpers. Its initial fast path admits one large, unfiltered
LZMA2 data member with a positional source and an explicit thread count
above one. Multi-member archives, finite budgets, adaptive/live controls,
hooks, filters and encryption use the existing ordered path. Planning
descriptors are bounded; all workers join on failure. Six native tests
cover byte equality, fallback contracts, CRC errors, path/link safety,
worker I/O failure, worker panic and short framing reads. The final scanner
checks source length, reads only chunk headers and skips packed payload using
the existing scanner operation. The initial combined x86 suite passed 458
tests in 20 targets, with zero failures and one ignored doctest. The later
header-skip refinement passed all six focused tests and all-feature Clippy.

The real `ArchiveReader::extract_to` comparison is complete. Both arms use
the same method, guarded file creation and final `sync_data`; a positional
source enables the ring arm. All 64 outputs matched 7zz by SHA-256 and
length. None of 2,766 load checks observed competing compiler processes.
One warmup and three measured repetitions per arm produced these medians:

| Case | Wall change | CPU change | RSS change |
|---|---:|---:|---:|
| compressed multi-run T2 | -3.04% | -1.13% | -86.09% |
| compressed multi-run T4 | -5.46% | -1.62% | -86.44% |
| compressed multi-run T8 | -8.93% | -2.37% | -86.59% |
| compressed multi-run T16 | -10.59% | -2.48% | -86.59% |
| single-run T2 | -0.52% | -0.33% | -70.61% |
| single-run T16 | -3.28% | -0.67% | -70.61% |
| near-incompressible T2 | -0.55% | -0.96% | -42.70% |
| near-incompressible T16 | -7.83% | +1.86% | -72.81% |

Every internal-path comparison stays below 3% regression in every metric.
The earlier public-API near-incompressible CPU exception remains recorded;
this distinct internal-path cohort needs no such exception. Evidence is
`x86-internal-ring-ab.{raw,report}.json.zst` in sevenz. Its source snapshot
predates the ordinary-reader selection below: the 122 MiB single-run
baseline must not be represented as the newer ordinary reader's footprint.

The ordinary seekable reader is also a candidate for internal ring use.
Its existing controlled LZMA2 decoder already has a ring, but the parallel
reader retains compressed read-ahead until it proves that only one run
exists. A bounded framing proof before coder creation can choose that same
ring immediately. The candidate extends the existing BCJ2 probe to
unfiltered standalone LZMA2, with at most 65,536 small framing reads and an
unconditional seek back afterward. Invalid framing, extra runs or a declined
probe retain the normal plan; adaptive sources are never probed. Three
native integration tests initially passed for ordinary/direct reads, cursor
restoration, adaptive and multi-run fallback, and damaged payload rejection.
The full x86 sweep then caught three failures across checksum-error and
folder-parallel tests: selecting the bare serial coder removed the existing
live progress report. That candidate is rejected, with its source snapshot
and failed full-suite log retained. The correction keeps the controlled
decoder and all its checksum, progress and ledger machinery; a proven lone
run enters its existing ring after the first input read. Existing regression
assertions remain unchanged. A new bounded read-ahead test covers this
contract. The corrected full x86 suite passed 474 tests across 26 targets,
zero failures and one ignored doctest; strict Clippy passed. Minimal and
utility-only library checks also passed on the Mac, without timing there.

The first controlled-ring cohort exposed an insufficient probe cap rather
than a decoder improvement: 4,096 framing reads did not reach the end of
the 938 MB packed fixture, so the hint declined and RSS stayed unchanged.
Partial samples are explicitly diagnostic in
`x86-ordered-ring-probe-cap-rejected.{raw,report}.json.zst`. The real scanner
needs 19,079 reads / 1,220,993 bytes to prove that fixture, taking 11 ms in
the diagnostic check. The final bound is 65,536 reads / at most 4 MiB of
snippets. The multi-run control still declines after 2,386 reads. A new
large-chunk-count test covers this cap. Final affected-contract validation
passed 264 tests across four targets, including the unchanged checksum and
folder-parallel assertions; strict all-target Clippy passed.

The final ordinary-reader cohort completed 72 samples and 12 before/after
digest checks. All archive CRC checks passed, and none of 3,626 load checks
found competing compiler processes. One warmup and three measured
repetitions, alternating baseline, candidate and 7zz, produced:

| Case | Wall vs baseline | CPU vs baseline | RSS vs baseline | Wall vs 7zz | CPU vs 7zz | RSS vs 7zz |
|---|---:|---:|---:|---:|---:|---:|
| single-run T2 | -0.52% | -0.51% | -67.15% | +0.15% | +0.15% | -67.01% |
| single-run T16 | -1.02% | -1.02% | -67.11% | -0.90% | -0.90% | -67.03% |
| single-run T2 streamed | -0.96% | -0.96% | -66.96% | +0.57% | +0.57% | -66.83% |
| multi-run T2 | +0.81% | +0.82% | +0.02% | +1.81% | +1.27% | +0.07% |
| multi-run T16 | -2.44% | -0.75% | +0.00% | -3.40% | +0.19% | +0.04% |
| near-incompressible T16 | +0.26% | -0.48% | +0.61% | +1.01% | -1.11% | -5.39% |

These rows pass all three 3% limits. Single-run RSS falls from about
123 MiB to 41 MiB. Evidence is sevenz's
`x86-ordered-ring-final-ab.{raw,report}.json.zst`. Middle-width controls also
passed: T4 changed +0.20% wall / +0.01% CPU / -0.01% RSS against baseline;
T8 changed +0.47% / +0.86% / +0.00%. Both remain within 3% of 7zz on
all metrics, with zero interference in 496 load checks. Evidence is
`x86-ordered-ring-midwidth-ab.{raw,report}.json.zst`.

The separate 64 KiB `BufReader<File>` comparison passed: -2.00% wall,
+1.66% CPU and -68.02% RSS, with eight byte-identical outputs and no
compiler interference in 672 load checks. Both arms used guarded extraction
and `sync_data`. Evidence is `x86-buffered-ordered-ring-ab.{raw,report}.json.zst`.

Further source review found that a seekable, non-adaptive custom source is
not necessarily complete. Unconditional framing inspection could wait on
future downloaded bytes before producing output. The final implementation
therefore requires explicit complete-input permission for both the ordinary
and BCJ2 probes and for positional ring extraction. `ArchiveReader::open`
sets it for its file; generic readers and positional block decoders default
to false. Custom completed sources can use `set_input_complete(true)`;
progressive readers retain their prior behavior. New regressions cover the
default no-probe contract, propagation through block decoding, and automatic
file-backed ring selection. The measurements above used complete local
fixtures and predate this permission gate. The final complete-input public
API comparison passes: +2.17% wall, +0.95% CPU and -68.04% RSS, with eight
outputs matching 7zz. Evidence is
`x86-complete-ordered-ring-ab.{raw,report}.json.zst`. The final combined
workspace sweep passed 481 tests across 26 targets, zero failures and one
ignored doctest; the default AWS-LC focused sweep passed another 235 tests.
Strict all-feature workspace Clippy passed. Mac timings remain deferred.

### Weaver positional integration checkpoint

The companion implementation now uses covered positional runs, independent
eligible folders on its caller pool, joined repair epochs, and guarded
private output with no-replace publication. Repair identity covers the
actual bounded parser-read ranges, including encoded headers outside the
usual archive tail. Partial publication retains original private aliases
and rolls back only aliases whose identity still belongs to this output.
Restart uses a fresh private generation and re-decodes; it never trusts a
crash-era CRC or completion record.

The final 28 focused tests, strict Clippy, formatting and diff checks passed.
An expanded intermediate cohort caught an unvouched-repair fallback error;
the corrected selection distinguishes missing proof from a vouched empty
prefix and enables replay only after the entire graph is admitted. The
original fallback assertion remains unchanged. Both failed and passing
evidence are retained in `weaver-positional-integration.{raw,report}.json.zst`.

The source-only patch excludes temporary local dependency and lock changes.
Its first signed commit failed because the signing agent could not fill its
input buffer; no signing requirement was bypassed. After the operator
unlocked the agent, the unchanged eleven-file patch was committed as
`082e5b3cc8922c70c4669c0f580b635089f06f91`; its SSH signature was verified.
No broad Weaver suite, e2e, Windows execution or Mac timing was performed.
Publication and release branch dependency integration remain outside this
implementation checkpoint.

The finite-budget COPY follow-on now preserves upstream owned delivery:
plain COPY hands off the existing charged 64 KiB pack buffer, and AES+COPY
hands off its owned plaintext buffer. Consumed AES
storage is zeroized and released before growth, so growth never retains
both old and new allocations. Allocation failure preserves input position,
CBC state and partial ciphertext for a correct retry. Unlimited COPY keeps
its previous buffer policy; unsupported upstream push stages retain their
caller-buffer fallback. The default AWS-LC focused suite passed 16 tests;
the native backend passed 13, with strict workspace Clippy and formatting.
The parent integrated the separate source patch after reviewing admission,
declared lengths and checksum behavior. The initial finite-budget AES buffer
growth policy is rejected: a 1.34% wall / 1.31% CPU win cost 24.10% more RSS.
That evidence remains in `x86-finite-copy-growth-rejected.{raw,report}.json.zst`.
The accepted finite-budget path caps AES push pieces at 64 KiB and preserves
owned delivery, CRC checking and retry behavior. Unlimited AES delivery and
Weaver's direct AES+COPY store implementation are unchanged.

One warmup and seven alternating measured repetitions per arm passed all
three limits: COPY changed -2.61% wall / -2.53% CPU / +2.19% RSS; AES+COPY
changed -0.17% / -0.09% / +1.27%. Four full-output SHA-256 checks matched
7zz, and timed delivery verified archive CRCs. Evidence is
`x86-finite-copy-compact-ab.{raw,report}.json.zst`. The combined workspace
and AWS-LC validation counts above include this final buffer policy.

### BCJ2 requested-input refill comparison

The SDK-directed refill cohort completed 156 samples and 26 digest checks,
with no competing compiler in 601 load checks. T2 and streamed T2 pass
all three SDK thresholds. Wider BCJ2 remains open: T4 CPU +10.95%, T8
CPU +10.27%, T16 CPU +4.12%; multi-run T2 wall +6.13% and T16 wall
+4.54%. The T8 candidate/baseline comparison is also +3.07% wall and
+3.55% CPU, so the source finding alone does not establish acceptance.
Other controls remain within 3% of their unchanged baseline. Preserve
this result and investigate the remaining wider scheduling cost rather
than claim closure. Evidence: `x86-bcj2-refill-ab.{raw,report}.json.zst`.

### Complete positional input for graph workers

The full input-path comparison with SDK `CLockedSequentialInStreamMT::Read`
found a caller-fed pipe absent from the reference path. Complete positional
graph inputs now give every pack stream an independent owned cursor. An
offloaded coder reads directly through a bounded 64 KiB input buffer; its
output pipe remains unchanged. The existing larger input-pipe reservation
is retained conservatively. Thread refusal falls back before moving input,
errors retain their stream order, and dropping the reader joins workers.
Incomplete and adaptive sources keep their borrowed input path. Weaver's
progressive direct AES+COPY store is unchanged.

The first cohort has 156 samples, 26 digest checks and 612 load checks with
no compiler interference. It improved RSS but showed +8.04% multi-run T16
wall time against baseline, with that short row varying between 148 and
173 ms across the two product arms. This result remains in
`x86-bcj2-direct-ab.{raw,report}.json.zst`; it is not silently discarded.

Confirmation used one warmup and seven measured repetitions per arm, with
216 samples, 18 matching digest checks and no interference in 483 load
checks. Every candidate/baseline metric passes the 3% threshold:

| Case | Wall vs baseline | CPU vs baseline | RSS vs baseline | Wall vs 7zz | CPU vs 7zz | RSS vs 7zz |
|---|---:|---:|---:|---:|---:|---:|
| single-run T2 | +0.82% | -0.23% | -1.72% | -4.20% | -1.68% | -6.36% |
| single-run T2 streamed | +0.22% | -0.12% | -1.85% | -3.11% | +0.52% | -6.38% |
| single-run T4 | -1.62% | -3.45% | -4.94% | -2.98% | +4.34% | -2.89% |
| single-run T8 | -1.16% | -2.48% | -4.87% | -1.87% | +4.22% | -2.49% |
| single-run T16 | +0.62% | -1.46% | -5.08% | -0.95% | +5.69% | -2.66% |
| multi-run T2 | -0.77% | +0.11% | -3.53% | +3.25% | -0.76% | -8.15% |
| multi-run T4 | -2.98% | -0.79% | -3.78% | -0.38% | -0.19% | -4.42% |
| multi-run T8 | +0.63% | +0.45% | -2.21% | -0.56% | +0.18% | -2.49% |
| multi-run T16 | -5.38% | -0.46% | -0.47% | -3.63% | +0.51% | -3.18% |

Evidence is `x86-bcj2-direct-confirm-ab.{raw,report}.json.zst`. The single-run
reference retains the agreed SDK mixer pairing; these are not literal
serial T1 comparisons. Remaining SDK misses are still open, without a new
tradeoff approval. Eighteen targeted pipeline tests pass, covering short
and interrupted positional reads, permission gating, worker error ordering,
thread refusal and early-drop joins. One intermediate test assumed the
corpus archive had one folder; it has two. The corrected test selects its
BCJ2 folder so independent folder parallelism cannot obscure the permission
being tested. The failed log is retained. The full combined workspace sweep
passed 485 tests across 26 targets, zero failures and one ignored doctest.
The default AWS-LC affected-contract sweep passed 250 tests across four
targets; strict all-feature workspace Clippy, minimal and utility-only
library checks passed. These are correctness gates, not closure of the
remaining SDK CPU rows.

Closure tracking is by exact workload, width, metric and reference. Passing
against the previous product build does not close a 7zz gap. In particular,
single-run BCJ2 T4, T8 and T16 CPU remain open until a measured build is at
most 3% above the same SDK reference; a ring RSS win or a different width's
result cannot close those rows. Multi-run BCJ2 T2 wall is also still open.
The next ownership-transfer comparison asserts the original product, SDK
and fixture hashes before measuring, and retains the direct-input build as
a separate control. The corresponding identities are:

| Artifact | SHA-256 |
|---|---|
| Original product baseline | `248b78f9ab1841f7d69155fdefaf53a94dd23051640f7159d903822b7deca6d7` |
| SDK 7zz reference | `3d52c92deb7e9f1bd059692eefc33f86a144cdc548acc4b6f4c809a4dd7bc369` |
| Single-run BCJ2 fixture | `68121d2fcdcd16c32a2cb22ccb40d50678617e7640d793da1c1d19bafd94799f` |
| Multi-run BCJ2 fixture | `7bbeca1d87ba0a72cf18b2593fbd13bf0a001b4efcf9a978cd3e45f5495feb8a` |

### Owned BCJ2 output buffers against the fixed reference

SDK `CStreamBinder::Write` offers the producer's storage to its consumer.
The product instead copied its owned output queue into another BCJ2 window.
`DecodeRead::take_chunk` now exchanges that queue allocation with the
consumer's spare; no borrowed pointers cross threads and no unsafe code is
added. Partial ordinary reads retain their cursor and defer ownership
transfer; partial BCJ2 words retain the ordinary-read fallback. Producer
errors follow the already-produced bytes, and dropped readers still join
workers. Allocation identity, mixed reads, EOF and error ordering have
dedicated tests.

The fixed-reference cohort has 416 samples (one warmup and seven measured
repetitions for the original product, direct-input control, new candidate
and SDK), 39 matching digest checks, and no compiler interference in 1,633
load checks. All 13 cases pass the candidate-versus-original and
candidate-versus-direct-input 3% regression gates. SDK misses remain:

| Exact row | Wall vs SDK | CPU vs SDK | RSS vs SDK | Closure status |
|---|---:|---:|---:|---|
| single-run T4 | -2.27% | +4.89% | -2.54% | Open: CPU |
| single-run T8 | -2.46% | +4.48% | -2.91% | Open: CPU |
| single-run T16 | -5.03% | +2.16% | -2.43% | Passes this cohort; held open after earlier misses |
| multi-run T2 | +4.57% | -0.75% | -7.05% | Open: wall |

Other SDK comparisons pass in this cohort. Evidence is
`x86-bcj2-chunks-ab.{raw,report}.json.zst`; the three product arms remain
separate so a baseline improvement cannot be called SDK parity. Full
workspace validation passed 487 tests across 26 targets, zero failures and
one ignored doctest. The default AWS-LC affected-contract sweep passed 252
tests across four targets. Strict all-feature Clippy and minimal/utility
library checks passed.

The remaining handoff audit found that SDK `CMixerMT::Init` negotiates
512 KiB pieces, whereas the product uses two queued 256 KiB pieces. Equal
queue capacity does not mean equal handoff frequency. A subsequent candidate
uses one queued 512 KiB piece only for complete positional graphs, charges
the larger active piece before admission, and sizes only connected pipeline
consumer windows to match. Progressive/adaptive input and Weaver's direct
AES+COPY store retain their existing policy. Its new budget test initially
assumed byte-granular admission; `ArchiveLimits::memory_limit_kb` rounds
upward. The corrected test explicitly checks both sides of that established
KiB boundary. Source, failed validation and corrected validation are retained.

The completed fixed-reference cohort rejected this candidate: 416 samples,
39 matching product digest checks and no compiler interference in 1,641
load checks. Against the accepted 256 KiB ownership-transfer build, single-run
T2 wall regressed 3.74% and T8 wall regressed 4.87%. Against the original
product, T2 wall regressed 4.84%. SDK CPU remained above the limit at
T4 +3.97%, T8 +3.05% and T16 +4.22%; it did not close those gaps. The candidate
also increased connected-graph RSS versus the accepted build by 0.63–2.83%.
Multi-run T2 wall was +2.97% versus SDK, a marginal result on rejected source,
not closure of the retained build's +4.57% row.

Only the 512 KiB experiment was removed. The affected decoder, pipeline,
reader and BCJ2 files were checked byte-for-byte and by hash against the
ownership-transfer snapshot that passed 487 workspace and 252 default AWS-LC
contract tests. Formatting and whitespace checks passed after restoration.
The benchmark source copy was restored as well; no additional full test
sweep was needed for an exact return to already-validated source. Evidence
is `x86-bcj2-handoff-rejected.{raw,report}.json.zst`, with the candidate source,
driver, failed budget test, corrected validation and load log in the lifecycle
bundle. The retained ring and Weaver AES+COPY changes are unaffected.

### Whole encoder-path audit before the next implementation batch

This inventory was completed before changing encoder production code. It
covers ordinary and encrypted 7z encoding from public writer options through
input, checksums, kernels, block scheduling, output delivery and metadata.
Reference: SDK 26.03, commit
`0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17`. Source correspondence is evidence
of a difference, not evidence that changing it closes a measured gap.

| Stage | Source comparison and disposition |
|---|---|
| Settings | `LzmaEncProps_Normalize`, `Lzma2EncProps_Normalize` versus `turbo_props`, `lzma2_block_plan` and `split_threads`: the measured L5 dictionary, finder, parser, fast-byte count and block size agree. Public defaults are intentionally different. Do not change library thread semantics to imitate the SDK's nominal T1. |
| Thread split | Normal BT divides requested threads between block coders and finder work; known short folders reduce the block count. The existing split agrees with the reference for the measured cases. |
| Size hints | SDK `CEncoder::Encode1` forwards expected size. The archive bridge does not forward a separate expected-size hint into LZMA2. That hint can change hash sizing and encoded bytes; a declared size is not a hard input limit. No speculative size-limit substitution. |
| Input ownership | Direct borrowed block input and owned fast-finder input already exist. Finder threads persist across blocks. Documentation describing these as missing is stale. |
| Generic reader bridge | The public reader and sink need not be `Send`. The push bridge preserves this contract; eliminating it by adding a bound is not a compatible optimization. Its input pieces are returned immediately after consumption. |
| Input storage | SDK reads into the coder buffer. The push bridge adds a copy into that buffer. The existing aligned input allocation and bounded queues have prior evidence; changing their sizes again requires profiling. |
| File and folder CRC | SDK `CFolderInStream::Read` computes each file CRC once. Both product solid entry points also hash all input for a folder CRC. File CRCs and lengths can be folded to preserve the folder checksum without rereading bytes. Include pulled, pushed and prepared-block paths. |
| Packed CRC | The product records compressed-stream CRCs where the SDK update path does not. Keep this integrity behavior; do not remove it to improve the comparison. |
| Source reader boundaries | Empty reads can currently finalize a member or advance `SeqReader`. `SourceReader` also uses CRC value zero as a finalization sentinel. Regression coverage is required before relying on per-file CRCs as the only bytewise checksum. |
| Finder layout | Hash/BT rings, local masked CRC tables and H2/H3/H3b/H4/H4b/H5/H5b heads already follow SDK structure. The native x86 walk, portable register walk, long-match continuation and whole-node copy are present. |
| Missing normalization kernel | SDK has explicit NEON/SSE4.1/AVX2 saturating-subtract normalization; the port uses scalar Rust that may autovectorize. Position normalization is not reached by these 32 MiB blocks. Record it as a separate long-stream opportunity, not the explanation for this gap. |
| Parser and prices | `GetOptimum`, `GetOptimumFast`, backward reconstruction and price update boundaries are already ported and differentially tested. No new algorithmic omission was established in this audit. |
| Range coder | Both use a 64 KiB output buffer. SDK keeps more range state in local variables in some symbol paths; Rust may promote the inlined field accesses. Inspect profiles/assembly before changing this code. |
| Chunk output | SDK `Lzma2Enc_MtCallback_Code` supplies `outBuf`; product streamed block callbacks still use the staged stream form. The existing memory API already has the direct `encode_subblock_into` port. Reuse that path for block workers. |
| Initialized output storage | Direct output must not eagerly zero every block on reuse or lose synchronous allocation return. Preserve initialized storage separately from the produced prefix, without exposing uninitialized bytes or adding an unchecked length restoration. |
| Block scheduling | Read token, read-before-semaphore ordering, size-dependent slot count, LIFO free slots, ready ordering and release-after-write agree with `MtCoder`. Do not reduce slots arbitrarily to hide RSS. |
| Thread lifetime | Block threads are scoped per encode call; SDK can park them across calls. Finder threads already persist. Any benefit concerns repeated folders, not a single large folder. Profile that workload before introducing another pool. |
| Output bridge | Whole blocks already transfer ownership and wait for synchronous delivery before reusing the same slot. This is retained work, not another missing-port discovery. |
| AES | AWS-LC already uses bulk CBC, a retained key, 64 KiB coalescing and fresh per-folder IVs. Existing profiles put AES below 0.31% of sampled cycles. No missing bulk kernel was found; Weaver AES+COPY direct store remains intact. |
| Finalization | Several codec variants discard errors from the downstream empty-write finalization. BCJ/BCJ2 and the pulled path propagate them. Add a failing-sink regression and propagate errors consistently. |
| Failure lifetime | Finder panic/cancellation and failed output handoff have tests. A separate `MtCoder` callback panic can bypass its completion accounting; a bounded reproduction is needed before selecting a lifecycle fix. Do not call that path verified by finder tests. |
| Small folders | Reduced dictionaries, retained pull coders, compact method records and pre-header release are already implemented. Fresh encrypted properties and a 64 KiB AES buffer remain allocations. Attribute their lifetimes before more layout changes. |
| Metadata | `UnpackInfo` serializes file/substream CRCs and shares identical coder descriptors. A combined folder CRC can be retained without changing serialized metadata or dropping integrity checks. |

Selected batch: direct block output using the existing SDK memory-output
port, eliminate duplicate solid-input checksum work, and protect the touched
paths with source-boundary and downstream-finalization tests. No parser,
finder, thread-count or queue-size tuning is included. The latter areas need
profiles if this structural batch leaves the timing/RSS rows open.

Validation must compare exact compressed payloads where deterministic,
decoded bytes and CRCs, short/empty input, stored chunks, allocation return,
read/write errors, encrypted padding failures and all relevant feature
configurations. Freeze the accepted product binary and source identities
before rebuilding: the release output left by the rejected BCJ2 experiment
is not a valid current baseline. Run correctness/lint gates before an idle
x86 cohort against both the accepted product and fixed SDK reference.
Retain raw failures, CPU, wall and RSS together. Mac and other fleet hosts
remain unverified for this batch; no row closes from a product-only win.

### Direct block output, checksum folding and whole-path profiles

The batch ports streamed block workers onto the existing SDK memory-output
path, retaining initialized output storage separately from its produced
prefix. Solid folder CRCs now combine already-computed file CRCs. Empty
reads no longer finish or skip source members, a real zero CRC no longer
acts as an unfinished sentinel, and downstream finalization errors propagate
through all seven finishing codecs. The finalization regression failed
before its fix. No unsafe code, dependency, parser policy or queue-size
change was added. The faithful output port is retained as part of the wider
path, even where it produces no immediate timing improvement.

One capstone validation passed formatting and strict Clippy, 407 lzma tests
(three ignored), 480 sevenz all-feature tests (one ignored), 457 default
AWS-LC tests and seven workspace-tool tests, with zero failures. The lzma
no-std check passed. These counts describe distinct configurations, not a
sum of unique tests. Further formatting and Clippy runs are reserved for
substantial batches, as requested by the operator.

The x86 cohort has 144 samples, one warmup and five rotated measured
repetitions per arm. All archives passed official integrity checks and all
plain product archives were byte-identical before/after. The 5,129 load
checks detected no compiler. A separate five-second interval measured about
0.35 core of background database activity; the host was not completely idle.
That limitation matters for marginal results. Candidate SHA-256:
`f1e768cd1847ce89657854b6eb992ac7401ddd5da462a055cc3a074c42635e09`.

| Case | Wall / CPU / RSS delta vs prior product | Wall / CPU / RSS delta vs SDK |
|---|---|---|
| L5 T4 | -0.40% / -1.13% / -0.08% | -7.91% / -5.27% / +1.18% |
| L5 T8 | -4.50% / +0.16% / +0.43% | -6.59% / +0.23% / +0.46% |
| L5 T16 | +0.48% / -0.34% / +0.59% | +3.08% / +4.55% / +0.47% |
| L5 T16 AES | -0.70% / +0.79% / +0.78% | +2.28% / +5.71% / +0.31% |
| L1 T16 | -2.26% / -1.54% / -0.70% | -12.01% / -13.70% / +1.79% |
| L3 T16 | -0.35% / -0.03% / +1.53% | -10.39% / -11.53% / +0.27% |
| L5 T2 | +2.71% / +2.46% / -0.57% | -11.65% / -3.25% / +2.95% |
| Tiny encrypted non-solid T1 | -0.14% / -0.10% / -5.61% | -18.37% / -17.52% / +12.50% |

The tiny encrypted row remains an unapproved timing/RSS tradeoff. T4/T8
RSS passing this cohort does not erase the earlier misses. Diagnostic
allocation tracing observed three resident candidate output slots versus
two in that reference run, explaining a roughly 28 MiB scheduling-dependent
peak; instrumented runs are not acceptance measurements. Tiny encrypted
anonymous residency was almost unchanged between product builds (6,276
versus 6,268 KiB); most of its observed RSS reduction was file-backed
residency, not reduced live allocations. SDK anonymous residency in that
diagnostic was 5,372 KiB. Do not attribute that RSS movement to CRC folding.

An additional 72-sample zeros cohort preserves L5 T2/T4 gains against SDK:
wall -23.23%/-19.02%, CPU -8.45%/-9.68%, RSS +2.69%/+1.12%. L5 T16 passes
SDK in that cohort but regresses the prior product by 3.64% wall and 26.20%
CPU, with substantial sample spread. It remains unresolved. Fast L1 T16
exposes a gap present in both product builds: candidate wall +8.64%, CPU
+93.52%, RSS +2.04% against SDK. Plain before/after archives match exactly.
The source and timing failures are retained rather than hidden by a new
baseline or used to discard the faithful port without attribution.

CPU profiles of those exact binaries, separate from timings, locate normal
L5 T16 work in the BT walk (49.49%), low-hash mixer (22.81%), parser (17.79%)
and hash producer (7.24%). The small structural output change is not the
dominant remaining work. On zeros L1 T16, 60.26% is in hash-chain skipping
and 29.50% in ahead-of-time hash-head preparation. The reference performs
the corresponding skipped-position updates in `Hc4_MatchFinder_Skip`.

The complete hash-chain follow-up audit covers HC4/HC5 match selection,
low/high hash updates, pending head consumption, skipped positions, chain
stores, cyclic wrap, refill/window movement, normalization and end tails.
The existing ahead-of-time heads are useful for dependent chain walks but
also run for positions already known to be skipped or fully matched. The
skip loop recomputes the high hash and dispatches head consumption per byte.
The grouped change now retains ahead-of-time heads for actual searches,
consumes any pending heads in order, and ports the SDK's direct batched hash
and chain updates for the remaining skipped positions. Full-length matches
perform the same single chain-head update without starting speculative
head preparation. The new bytewise finder differential test compares future
matches and logical state across queued/direct transitions, window/cyclic
boundaries, HC4/HC5, empty/short input and normalization. It also exercises
the C's unsigned position wrap without allocating a four-GiB fixture.
All six finder tests and 16 required native encoder/oracle tests passed;
two heavy guard-page cases remained ignored. No unchecked accesses were
added. The separate frozen candidate is
`2553b63478cafbb85a59919eb3ad455cc8e7eb3bf6be2cd195c1d619dd838064`.
Its four-arm cohort preserves the accepted product, direct-output-only
product, grouped candidate and fixed SDK as distinct comparisons.

The small-archive decode audit also reconfirmed that the decoded-header
dictionary is capped, decoded header storage is reserved exactly once,
the header decoder dies before metadata parsing, and the LZMA input buffer
is 64 KiB. Sequential extraction does not build the lazy filename index.
SDK stores names in one UTF-16 blob; the public product API exposes owned
strings and coder vectors. The remaining metadata gap cannot be credited
to a nonexistent extra input buffer or fixed by dropping public fields.

Evidence in sevenz: `x86-encoder-direct-output.{raw,report}.json.zst`,
`x86-encoder-zero-controls.{raw,report}.json.zst`, and
`x86-encoder-direct-output-validation.json.zst`. The validation bundle keeps
source snapshots, drivers, failures, gate logs, allocation traces and CPU
reports; its decoded SHA-256 is
`2e4757aabb298cce752d2e6795da3d20b36b5b15627d68ca8b1e8c695d98de91`.
Native raw CPU profiles remain in the benchmark workspace with hashes in
that bundle. Mac measurement permission resumes at 01:00 local; no current
Mac acceptance is claimed.

### Grouped hash-chain port: four-arm x86 controls

The 144-sample cohort completed with five measured repetitions after warmup
for each arm. All archives passed SDK integrity checks; all three product
builds emitted identical plain archives. The 1,775 load checks detected no
compiler, but a separate two-second sample found about 0.4 core of background
database work. Marginal ratios remain subject to that limitation.

| Case | Wall / CPU / RSS delta vs direct-output-only product | Wall / CPU / RSS delta vs SDK |
|---|---|---|
| Zeros L1 T16 | -25.84% / -58.04% / +0.10% | -19.15% / -19.10% / +2.40% |
| Zeros L1 T2 | -49.01% / -48.26% / +0.37% | -19.68% / -16.51% / +8.32% |
| Zeros L3 T16 | -26.00% / -59.54% / +0.06% | -19.08% / -15.75% / +7.32% |
| Mixed payload L1 T16 | -0.62% / -1.83% / +0.05% | -12.11% / -14.78% / +1.94% |
| Mixed payload L3 T16 | -7.97% / -7.92% / -0.13% | -9.91% / -11.49% / +0.49% |
| Zeros L5 T16 | +0.29% / +2.09% / -0.03% | -16.02% / -20.59% / +0.41% |

Zeros L1 T16 now passes all three SDK thresholds in this cohort. Zeros L1 T2
and L3 T16 are proposed timing/RSS tradeoffs, not approved exceptions. Their
RSS was essentially unchanged by the HC port. Mixed-payload controls show
no new timing regression from preserving the SDK skip path alongside the
existing ahead-of-time heads used for actual searches.

Zeros L5 T16 still uses 20.28% more CPU than the accepted pre-output baseline
(wall +2.29%, RSS -0.14%). The HC delta is only +2.09% CPU against the
direct-output-only build. A reference win does not close that product
regression. The threaded BT consumer, skip, normalization and producer
audit found the corresponding SDK structure already present; per-core
profiles of these exact builds are the next diagnostic, without changing
affinity, thread counts, queues or kernels speculatively.

Raw and median evidence: sevenz
`x86-encoder-hc-controls.{raw,report}.json.zst`. The decoded raw SHA-256 is
`14597a4e02321672ce8bd9cc59504b6e0f388c86e68d6090c0d3d2535b31236e`;
the report SHA-256 is
`a71f91500d138e195f20c625951ce7e4c6cad040f731c3e0af058ec67c4c3470`.

### Threaded finder layout audit before the next port

Per-core samples put the pre-output baseline at 3.277 P-core / 4.737 E-core
CPU seconds and the direct-output build at 4.341 / 6.220. Both classes get
slower; this is not simply work moving onto E cores. A diagnostic restriction
to P cores removes the direct-output CPU difference (7.150 versus 7.147
seconds, three measured repetitions). This is attribution, not acceptance
and not justification to impose affinity on library callers. Hybrid PMU
counts from the initial counter run are scaled separately per PMU and must
not be summed as actual instructions or cycles.

The complete shared-layout comparison found an omission from the earlier
algorithm inventory. SDK `CMatchFinderMt` places a 128-byte `btDummy` between
LZ/BT synchronization and BT-owned state (`kMtCacheLineDummy` explicitly
requires at least a cache line). Hash-owned state lives in the separate
`CMatchFinder` allocation. The port's `HashState`, `BtState`, `LzState` and
two `MtSync` objects have ordinary alignment inside one `MtShared` allocation;
Rust may place separately written fields on the same cache line.

The audit covers all five frequently changed groups, shared read-only
`Common`, block input, allocation ownership, shift/error publication, and
the separate low-hash/high-hash/son and ring-buffer partitions. The latter
already preserve SDK table/ring boundaries; their algorithms and storage
sizes will not change. Shift publication is infrequent and remains atomic.
The selected port gives each state and synchronization object a complete
128-byte-aligned extent, preserving the SDK separation despite Rust field
reordering and the hash state now living inside the shared allocation.
No thread, queue, synchronization-ordering or kernel algorithm change was
needed. The layout regression failed before the port: hash state occupied
offset 1200 / size 48, BT 0 / 32, LZ 1432 / 40, and the two handshakes
1248 / 80 and 1328 / 80. After isolation, all 19 native threaded-finder tests
and 15 encoder/SDK oracle tests passed. A combined capstone and a separate
frozen-binary cohort follow; no timing improvement is claimed from layout
inspection alone.

The expanded bytewise boundary oracle also passes for all six finder
families. Its initial BT failure was a test-harness violation of SDK Skip's
positive-count precondition, not normalization: the production parser already
guards zero-length moves. The corrected oracle retains no-skip steps without
calling `Skip(0)`, and both logs are preserved. Completed HC diagnostics,
sources, drivers and failures are in sevenz
`x86-encoder-hc-validation.json.zst`, decoded SHA-256
`6cf2249b3a1488461af5882dff2b8c0df019f78dc9322f2e41bc01b9b8a8d8c7`.

The combined finder capstone passed formatting, strict Clippy, 409 lzma
tests (three ignored), 480 sevenz all-feature tests (one ignored), 457
default AWS-LC tests, and the no-std check, with zero failures. The counts
are separate configurations, not unique-test totals. The frozen layout
candidate is
`b4191dd8982709aa5c9533e6b592ec59a96179aa98974eb91592dca393841998`.

### BCJ2 full-path revisit before further changes

This is a source inventory while the encoder cohort runs, not a new BCJ2
implementation. Read the complete pipeline owner/wait/cancel path alongside
SDK `CMixerMT`, `CCoderMT`, `CStreamBinder`, `Bcj2Dec_Decode` and the BCJ2
reader bridge. Preserve the agreed SDK mixer pairing and literal library
T1 behavior. Current-build profiles must decide the next batch because
earlier notification and buffer-size candidates did not close the gaps.

| Stage | Current source and reference finding |
|---|---|
| Admission and graph | `build_decode_stack2`, recursive graph wiring and `offload_plan_with_serial` validate counts, preserve memory admission, and select one parallel LZMA2 coder. A proven complete lone run uses the serial ring. Progressive/custom sources retain the permission gate. |
| Packed input | Complete positional leaves already go directly to workers with independent bounded cursors and 64 KiB buffering. Borrowed, non-Send sources still need caller pumping. Removing that coordination globally would break the supported input contract. |
| Queue storage | Owned 256 KiB chunks are recycled without clearing initialized storage; BCJ2 takes ownership when there is no partial word. Ordinary reads and partial words preserve their cursor/fallback. No second copy is hidden in the ownership-transfer path. |
| SDK binder copies | `CStreamBinder::Write` lends the producer buffer until consumed; its `Read` still copies into the consumer buffer. Treat it as one copy, not a wholly zero-copy SDK path. The existing owned product handoff removes the extra queue-to-BCJ2 copy. |
| Synchronization | SDK binders have independent per-edge event/semaphore pairs. Product pipes share one `Mutex<State>`, generation and `Condvar::notify_all`. The preserved rejected wake patch added per-pipe condvars but retained that global mutex and the global notification; it was not a complete port of independent binders. |
| Queue geometry | SDK negotiates one 512 KiB producer buffer. Product holds two queued 256 KiB pieces plus its active piece. The earlier 512 KiB candidate regressed timings and is preserved. A size-only retry has no new evidence. |
| Refill | The bridge already refills only `required_input`, retries interrupted reads, retains partial side-stream words and checks unused side coders at completion. SDK `CBaseDecoder::ReadInStream` is the correspondence. |
| Kernel | Priming, range normalization, probability/branch selection, partial target output and final state follow the C decoder. The product uses grouped eight-byte marker scanning plus bulk copy; SDK uses four unrolled `ONE_ITER` steps. Their different cost on branch-dense input needs attribution before changing either path. |
| Checksums and length | Graph output retains `DeclaredLength` and optional folder CRC; file CRC remains on the final filtered bytes. Worker checksum folding is deliberately disabled below BCJ2, where bytes are not yet final. Preserve these checks. |
| Failure lifetime | Reserved threads precede moving input; refusal falls back to the caller. Produced bytes precede errors. Panic publication wakes readers, and abandonment wakes and joins all stages. Any binder port must retain these behaviors for complete and progressive sources. |

The prepared diagnostic profiles cover single-run T4/T8 and multi-run T2,
against the fixed SDK and accepted product, after the encoder measurements
finish. No new BCJ2 source change or performance claim follows from this
inventory alone.

The bridge comparison also includes SDK `CDecoder::Code`, `Read`, and
`SetOutStreamSize`, not just the low-level C loop. The product's public
`Read` keeps its empty-buffer no-op contract; SDK uses zero-length reads
as an internal initialization/finalization signal. Those are distinct API
contracts, so copying the SDK call literally would not be a safe port.
Required-input refill and final side-coder validation preserve the product's
current behavior. The next profiles include kernel samples and a separate
system-call summary: user-only samples cannot attribute binder waiting.

The encoder output-lifetime revisit covers `ThreadFunc2` from read-token
acquisition through input ownership, block admission, free-list selection,
ordered writes, recycling and error exit, then the sevenz writer bridge.
Block admission and LIFO reuse follow SDK. The bridge waits for delivery
and returns the same initialized allocation to its original slot; there
is no duplicate block allocation outside that budget. An additional
resident output slot can still arise from scheduling, as the earlier
allocation trace showed. Neither reducing queue depth nor discarding
reusable buffers has been justified by this audit.

Faithful ports are retained as a combined implementation even when their
isolated timing effect is small. Each frozen comparison remains separate:
the pre-output product, direct-output port, grouped HC port and SDK cache-line
separation. This preserves both useful components and evidence of unresolved
regressions; a new reference win does not erase an older product regression.

### Combined encoder ports: completed x86 controls

The four-arm cohort completed 264 samples (warmup plus five rotated measured
repetitions per arm/case), with 6,883 load checks and no detected compiler
interference. Background database CPU remained present, so this is not a
claim of an otherwise perfectly idle host. Every archive passed SDK integrity
checks; plain archives were identical across all three product binaries.
The table gives median deltas for the layout candidate against the fixed SDK.

| Case | Wall | CPU | RSS |
|---|---:|---:|---:|
| Zeros L5 T2 | -21.37% | -7.77% | +2.85% |
| Zeros L5 T4 | -20.59% | -11.09% | +1.23% |
| Zeros L5 T16 | -0.13% | -6.26% | +0.41% |
| Mixed payload L5 T2 | -14.46% | -7.88% | +3.28% |
| Mixed payload L5 T4 | -6.80% | -3.13% | +1.20% |
| Mixed payload L5 T8 | -2.85% | -0.61% | +5.33% |
| Mixed payload L5 T16 | +1.69% | +3.52% | +0.34% |
| Mixed payload L5 T16 AES | +4.04% | +4.11% | +0.34% |
| Tiny-file tree L5 T1 AES | -22.28% | -22.23% | +14.94% |
| Mixed payload L1 T16 | -14.86% | -17.25% | +1.89% |
| Mixed payload L3 T16 | -9.31% | -12.49% | +0.49% |

T2/T8 mixed-payload RSS and normal/encrypted T16 timing remain open. T4
passes this cohort, but earlier extra-slot residency observations remain
part of the record. Tiny encrypted files are a proposed timing/RSS tradeoff,
not an approved exception. Zeros T16 still uses 16.42% more CPU than the
pre-output product, despite passing SDK; layout isolation reduces CPU 2.71%
against the HC build and is retained. No row is declared universally closed
across architectures from this host's measurements.

Evidence in sevenz: `x86-encoder-layout-controls.{raw,report}.json.zst` and
`x86-encoder-layout-validation.json.zst`. Decoded SHA-256 values are
`9b4bbe7a267ab0f452056360d6b58ffdc2a34fe89b7c8f3c956123bc17a6431d`
(raw), `db1f8b794c8de02ded195c7ea75aeea6abef1f0d31db4078c0c7c45677875def`
(report), and
`a59220cfc6db90695a2f43e89ed0a1afac278d47cb3921d9149078ce0d65fcef`
(validation). The latter preserves 815 logs/results/drivers and both frozen
source snapshots, including the failing layout regression before its fix.

### Profiled BCJ2 SDK copy-loop port

The full-path inventory above was followed by user/kernel CPU profiles of
single-run T4/T8 and multi-run T2. LZMA decoding dominates; pipe/futex self
samples are about 0.1–0.3%, while the grouped BCJ2 converter accounts for
about 8–9%. The SDK converter's exact sampled-period shares are 6.25%,
7.64% and 6.22%, respectively. Its stripped address range was identified
from disassembly matched to `Bcj2Dec_Decode`; thresholded human reports
understate that share. Separate syscall diagnostics are intrusive and are
not acceptance timings. This evidence does not justify a binder rewrite.

The production main-copy loop now follows the SDK's four `ONE_ITER` steps
and short tail, retaining the existing bounded slices and state machine.
The previous grouped implementation remains a test-only differential
oracle, alongside the independent bytewise oracle. No unsafe, dependency,
threading or public contract change was needed. Kernel differentials,
SDK oracle, bridge/archive tests and no-std checks passed on x86.

The frozen candidate is
`692327ea42aef1cfa31c88d9310bd8c8d9de0ae004e885a3c53007ecfde1752f`.
The cohort completed 312 samples, one warmup plus seven rotated measured
repetitions, and 1,199 compiler-interference checks with none detected.
Product output digests matched. Existing background-host caveats still
apply. Median deltas below use the unchanged SDK pairing.

| Case | Wall vs SDK | CPU vs SDK | RSS vs SDK |
|---|---:|---:|---:|
| BCJ2 T2 | -4.21% | -3.13% | -6.00% |
| BCJ2 T2 stream | -3.99% | -3.06% | -6.01% |
| BCJ2 T4 | -2.01% | +3.46% | -2.32% |
| BCJ2 T8 | -2.81% | +1.02% | -2.67% |
| BCJ2 T16 | -4.12% | +0.04% | -2.23% |
| BCJ2 multi-run T2 | +1.32% | -2.28% | -6.94% |
| BCJ2 multi-run T4 | -2.06% | -0.75% | -4.19% |
| BCJ2 multi-run T8 | -2.20% | -0.07% | -2.40% |
| BCJ2 multi-run T16 | -12.78% | -1.55% | -3.59% |
| AES stored T2 | -51.45% | -46.47% | -15.60% |
| ARM64 filter T1 | -4.75% | -7.93% | -3.99% |
| Compressed T16 | +1.54% | +0.38% | +0.02% |
| AES near-incompressible T16 | +3.50% | -2.09% | -8.65% |

Single-run BCJ2 CPU falls 2.09–3.18% against the preceding product;
multi-run T16 wall falls 11.08%. BCJ2 T4 CPU and encrypted
near-incompressible T16 wall remain open. The latter code path did not
change in this port; neither a marginal miss nor a control's small movement
establishes causality. The faithful port is retained as part of the combined
implementation, without claiming every architecture is now closed.

Sevenz evidence: `x86-bcj2-sdk-loop-controls.{raw,report}.json.zst` and
`x86-bcj2-sdk-loop-validation.json.zst`. Decoded SHA-256:
`54149d02674cd11e7ca827199c2c6c302aa34129569cc80fc3962974ead5040b`,
`040ca102b5bcebe45d13adb88e92f9f340a68687275e536edc04a44d4effd6ba`,
`164283645c4f335d26fe8656381c7085fef497d0a4e391732fb35a3370e4ee5d`.
The validation bundle retains 1,011 text artifacts, source snapshots,
profile hashes and the original missing-fixture/collection failures.

### ARM combined capstone and interrupted timing cohort

The preceding combined build (before the BCJ2 copy-loop port) passed strict
Clippy, 404 lzma Nextest cases (two skipped), 482 sevenz all-feature cases,
467 default AWS-LC cases, both doctest suites and the no-std check. Counts
refer to separate configurations. Frozen binary:
`ec06d62d856a30c0709c0df3574c42b539f38b8d2d795f19d62101da5b6b2099`.

Other compilation resumed during the encoder timing cohort. Only our
benchmark processes were stopped; the 110 partial samples are preserved
as contaminated diagnostics, with no acceptance or regression claim.
The new BCJ2 loop still needs ARM validation and quiet-host measurements.
Sevenz evidence: `mac-combined-encoder-interrupted.{raw,report}.json.zst`
and `mac-combined-encoder-interrupted-validation.json.zst`. Decoded SHA-256:
`cf8af5766b8dd716d6fd45ddd27bee2e9a4c5c62025d7c4387a31c33d4338cd4`,
`89ffcc9056b1c49f2e2ea1518ef1c6a601ea9b0d1632a9619c7b778fa896fa8f`,
`56c65edd0205740152844d874beead6dca1e87bd2fdbf89d07bc1c889b082123`.

The subsequent BCJ2 ARM check passed ten kernel cases, five SDK parity
cases, six bridge cases, eleven archive cases, no-std and the release build.
It produced binary
`344d0df0360f341b27ec0831ad54597f364ec6d0300bf8776b274efce5db4400`.
Quiet-host timing remains outstanding. Evidence:
`mac-bcj2-sdk-loop-validation.json.zst`, decoded SHA-256
`a8a00029a746fcee52c691b11fe63f6228eb9a43412b7b2ca6a9271dfb954e99`.

### Encoder allocation policy after whole-path attribution

New profiles of the frozen combined encoder cover zeros L5 T16 and
encrypted payload L5 T16, including kernel samples and CPU placement.
AES is only 0.23% of candidate samples. The BT walk is 50.70%, low-hash
matching 22.19%, and the parser 15.49%. The complete BT assembly listing
matches the preceding product after address normalization. This is not
evidence for a missing AES kernel or a reason to rewrite the copied walk.
Annotated low-hash samples concentrate around table reads/stores and history
loads; sampling skid prevents assigning that latency to one instruction.

The T8 allocation diagnostic found four identical-sized finder tables and
four input blocks in both implementations. Five product output buffers
versus six reference buffers made diagnostic RSS lower in the product,
the reverse of the acceptance cohort. This strengthens the observation
that output-slot residency depends on scheduling; it does not close the
timed T8 RSS miss. Full `ThreadFunc2`, free-list and ordered-delivery audits
found no unjustified extra owner to remove. Product packed CRCs remain
intact: SDK `CEncoder::Encode1` counts output bytes without that CRC pass,
but dropping our integrity metadata would change product behavior.

The allocation-source inventory covers SDK `BigAlloc`, aligned allocation,
failure fallback and advice, product zeroed allocations, streamed block
input/output owners, finder transfer/reuse, and memory-limit estimates.
SDK aligns large tables to 2 MiB before advice. Product advice previously
covered only the aligned interior of an ordinary allocation. All four
traced product tables started about 1.74 MiB into a huge page; all four SDK
tables were aligned. Consequently the hot low-hash region missed the
product's advice. Existing source-input alignment does not fix these tables.

A disposable allocator diagnostic intercepted only the traced L5 table
size. Alignment alone changed CPU by +0.14%; alignment plus pre-write advice
changed CPU by -2.32%, wall by -0.95%, and RSS by +0.25%, with byte-identical
archives. These are three-run diagnostic medians, not product acceptance:
the shim eagerly initializes tables, unlike the library's lazy allocation,
and an ordinary-arm outlier limits confidence. The diagnostic is preserved,
not installed as a product allocator.

The selected production port keeps a normal zeroed Vec as the owner and
aligns its logical table inside bounded padding. It retains lazy page faults,
the original allocation/deallocation layout, and table ownership through
finder switches and threaded runs. Advice covers only the logical table,
excluding padding; actual and estimated allocation costs include padding.
Optional padded allocation falls back on refusal, while real allocation
failure still returns `Error::Alloc`. No new unsafe or dependencies are
introduced. Tests cover zeroes, alignment, moves, untouched padding,
fallback, failure, finder differentials and memory-budget accounting.
This is a port candidate pending its own production-binary measurements,
not a closure inferred from the diagnostic. Existing windows, input/output
exchange and handoff storage retain their owners and policies.

The first targeted command named a nonexistent `lzma2_parity` target;
its other gates passed. The corrected combined capstone runs the complete
workspace suites, retaining the command failure in evidence.
Attribution evidence: sevenz `x86-encoder-layout-attribution.json.zst`,
95 text artifacts and six retained raw-profile hashes, decoded SHA-256
`11987fdcc283bca8858ff5adec8679c3ff5345e92441f604620af4373316301b`.

The ownership capstone passed both formatting/strict-Clippy gates, 411 lzma
tests (three ignored), 487 sevenz all-feature tests (one ignored), 472
default AWS-LC tests, and no-std. The final logical-range advice adjustment
then passed 207 library tests, 18 encoder/parity/allocation cases, no-std,
formatting and strict Clippy. Both focused table-owner tests passed on ARM.
Counts are separate configurations; the ARM timing gate is still pending.
The measured candidate is
`ad6a3fb8405a36f4494615060d82de7e807db08dda6fa8b8d5ddc75bda338219`.
All command logs, the earlier target-name error and both source snapshots
are in sevenz `x86-table-alignment-validation.json.zst`, decoded SHA-256
`c76a477e3f7a0eada1c6b4bd4f25bb78d3bda5cba5a711c145652e81f47332d0`.

The production table-alignment cohort completed 156 samples: a warmup and
three measured repetitions of each of three arms across thirteen cases.
Every plain product archive matched the preceding product byte for byte;
all archives passed the SDK integrity check. The 3,551 compiler-load checks
are retained alongside snapshots showing a small unrelated background
service load. These results describe this x86 host, not every architecture.

| Encode case | Wall vs SDK | CPU vs SDK | RSS vs SDK |
|---|---:|---:|---:|
| zeros L5 T16 | +0.69% | -13.01% | +0.41% |
| payload L5 T4 | -12.62% | -9.21% | -7.33% |
| payload L5 T8 | -2.08% | -1.54% | +0.11% |
| payload L5 T16 | +2.78% | **+4.35%** | +0.45% |
| payload L5 T16 AES | **+3.29%** | **+4.19%** | +0.13% |
| zeros L5 T2 | -25.46% | -12.23% | +2.84% |
| zeros L5 T4 | -23.68% | -12.69% | +1.14% |
| payload L5 T2 | -13.45% | -6.02% | +2.92% |
| tiny AES non-solid L5 T1 | -20.36% | -21.65% | **+6.16%** |
| payload L1 T16 | -15.38% | -14.67% | +1.72% |
| payload L3 T16 | -8.32% | -10.13% | +1.03% |
| zeros L1 T2 | -26.50% | -22.15% | **+7.50%** |
| zeros L3 T16 | -19.90% | -17.29% | **+7.37%** |

T8 improved by 9.38% wall and 10.75% CPU against the preceding product.
T16 gained only 0.25% CPU; its CPU miss and encrypted wall miss remain open.
Payload L3 T16 was 3.94% slower in wall time than the preceding product,
despite remaining faster than SDK; that observation is retained for follow-up.
The older zeros T16 CPU regression against the pre-output product and prior
RSS misses remain in the ledger. A new median does not erase them. The
three bold RSS cells with large timing wins remain proposed tradeoffs.
The faithful table port is retained as part of the allocation-policy port;
its value is not judged solely by one T16 median.

Evidence: sevenz `x86-table-alignment-controls.{raw,report}.json.zst`;
decoded SHA-256 respectively
`bf1a45661db8322d1b508d01f59a5c0312fbf767d5b00302c20320521827ec93`,
`83a5d2cf6a8a6942621bc15101b2b7beb17591616b17e3ff69cdbe9acc8918f9`.

### Completing the SDK allocation ownership audit

The remaining `BigAlloc` inventory is the streamed block input, ordinary
finder window and threaded hash/BT handoff storage. The complete path audit
covers first allocation, short reads and EOF, direct-input borrowing, owned
input exchange with the fast finder, window slides, finder-mode switches,
thread cancellation, failure parking, reuse, output errors and memory limits.
Output buffers use the separate delivery contract and are not reclassified.

A four-arm diagnostic compared ordinary input/ring allocation, aligned
input, aligned rings and both. Three-run medians changed CPU by +0.61%,
+0.95% and +1.15%, respectively; wall changed by +0.18%, +1.21% and +1.88%.
RSS stayed within 0.14%. All archives were identical and passed SDK checks.
The shim eagerly initializes allocations, so it cannot establish the value
of a lazy production owner. Its unfavorable result is retained, not called
a win. Evidence: sevenz `x86-table-alignment-followup.json.zst`, decoded
SHA-256 `dc45a6c53442e8cc10282bbfb92ba317edf2ab816ccd72339a8027e5fe83d026`.

A fresh profile of the table-only binary has 99.38 sampled CPU seconds
against SDK's 95.46. Complete verified address ranges attribute 51.44 vs
50.23 seconds to the respective BT kernels, 21.38 vs 20.07 to low-hash mixing
and its wrapper, and 7.98 vs 7.33 to high-hash production. The residual is
spread across the pipeline; synchronization and AES are not dominant.
P/E placement and unthresholded samples are retained. These are diagnostic
samples, not replacement acceptance times. Sevenz evidence:
`x86-aligned-table-profile.json.zst`, decoded SHA-256
`90efbe122cfde97d04a78fb3cc928392c8ab1ea2a16a32e91c99f27a2b722c06`.

The combined production candidate extends the existing safe aligned owner
to all three remaining SDK allocation classes. It keeps allocator-provided
zeroes lazy, retains the original Vec deallocation layout, bounds advice to
logical data, and returns the same input owner after coding errors. Restoring
a short input extent zeroes only the newly exposed logical bytes. Padding
is included in new and retained-window memory accounting; output ownership,
worker scheduling, CRCs and public thread/Send contracts are unchanged.
No additional unsafe or dependency is introduced. The faithful port stays
reviewable as a unit rather than being judged by the eager shim alone.
Its own production measurements remain required.

The first x86 compile caught a test's implicit raw-pointer autoref; the
explicit borrow fixes that test without changing the safety argument.
Subsequent library, SDK parity/allocation and no-std gates passed. The frozen
combined candidate is
`392a78088f8ca7d32b95bce9c47da8b0f0a2afb408dde28a0c9f3d25a58b131a`.
The ARM capstone passed strict Clippy and 407 tests with two skipped tests;
this is correctness validation, not a quiet-host timing result.
The sevenz ARM capstone also passed strict Clippy, 486 all-feature tests and
471 default AWS-LC tests. X86 validation passed 208 library tests and 18
encoder/parity/allocation cases. Evidence, including the original compile
failure and complete source snapshot: sevenz
`allocation-policy-validation.json.zst`, decoded SHA-256
`89845f6eb6430b819010add38b2fb3b0bcdbb8ebc9522ffbb0eca908c1eed51e`.
The formatted local source differs from the measured x86 snapshot only in
test formatting. A later Mac load check still found VM/browser CPU activity
and substantial compressed memory; no Mac timing closure is claimed.

The combined allocation cohort completed 156 samples and 3,395 compiler-load
checks without detected compiler interference. Every plain product archive
was byte-identical across both product arms; all archives passed SDK checks.

| Encode case | Wall vs SDK | CPU vs SDK | RSS vs SDK |
|---|---:|---:|---:|
| zeros L5 T16 | +0.92% | -5.95% | +0.41% |
| payload L5 T4 | -2.34% | +2.80% | -7.50% |
| payload L5 T8 | -5.67% | -1.48% | +0.45% |
| payload L5 T16 | +2.63% | **+6.40%** | +0.47% |
| payload L5 T16 AES | +2.74% | **+4.32%** | +0.18% |
| zeros L5 T2 | -19.35% | -4.78% | +2.89% |
| zeros L5 T4 | -23.33% | -13.16% | +1.34% |
| payload L5 T2 | -12.94% | -5.58% | **+3.28%** |
| tiny AES non-solid L5 T1 | -14.86% | -15.46% | **+14.78%** |
| payload L1 T16 | -7.77% | -11.96% | +1.91% |
| payload L3 T16 | -7.19% | -9.34% | -0.47% |
| zeros L1 T2 | -22.67% | -19.70% | **+8.23%** |
| zeros L3 T16 | -19.78% | -15.78% | **+14.51%** |

Relative to the table-only port, T8 improved by 1.92% wall and 3.42% CPU,
with 4.19% higher RSS. T4 instead regressed by 5.22% wall and 5.96% CPU,
with 8.56% lower RSS. Payload T2 regressed by 3.49% wall and 4.24% CPU;
L1/L3 T16 wall regressed by 5.31%/4.83%. These observations remain open,
even where SDK ratios pass. Full-width plain CPU changed by +0.79%, and
encrypted CPU by -0.01%; this port does not close the full-width CPU gap.
Encrypted wall passing in this cohort does not erase the earlier miss.
The RSS cells with timing wins remain proposed tradeoffs, and their full
spread across cohorts is retained. No universal or release-ready claim is
made for this candidate. The faithful allocation implementation remains
available for the combined investigation rather than being discarded on
the strength of one isolated measurement.

Sevenz evidence: `x86-allocation-policy-controls.{raw,report}.json.zst`,
decoded SHA-256 respectively
`9869be40d47ad98c5e8364eaead56f526270e97fbce1c13b8e74cb1917268f7e`,
`1f02228e6640a3f52b7b91f45137b7110b21024879fb0f27008bdae43fe6963c`.

Separate cache/TLB profiling followed the timing cohort, never overlapped
it, and retained a warmup plus three runs per arm for payload and zeros.
P-core and E-core counters remain separate: perf scales them according to
different runtime fractions, so their displayed totals must not be added.
Rates below compare arms within a PMU, not P-core instructions with E-core
micro-ops. Timing outliers remain in the evidence.

On payload, the combined port changed page walks per cycle by -13.21% on
P-cores and -19.99% on E-cores relative to the table-only port. L1 misses
per cycle changed by -4.10%/-5.53%, and L2 misses by -3.16%/-2.49%.
Diagnostic CPU nevertheless changed by +0.70%. Thus the allocation port has
a measurable translation/cache effect without closing the CPU gap; this
is not evidence for discarding the faithful implementation or for declaring
it faster. Zeros still has substantially higher E-core L2 misses per cycle
than SDK, but sampled rates alone do not identify a causal instruction.

The next source inventory covers the whole hash producer and lz consumer:
`GetHeads*`, `MixMatches*`, `MatchFinderMt_GetMatches`, specialized skips,
held-block boundaries, normalization, failure parking and parser callers.
Their algorithmic SDK counterparts are already present. Differences
include checked indexing and reconstructing block views in
Rust wrappers; these are candidates for further attribution, not permission
to remove bounds checks without a complete margin proof and differential
coverage. Reconfiguration was also checked end to end: `sync_coder` already
rebuilds stale oversized coders under a finite memory limit, so that safeguard
is preserved rather than rewritten.

Sevenz evidence: `x86-allocation-cache-profile.json.zst`, decoded SHA-256
`5eead92a0ef81ce1fa573de55f7c574fe8c6a78573d07cf048423ce763c89d6b`.

The subsequent complete assembly-source comparison corrects an important
attribution boundary: the BT listing was identical between two product
revisions, not between product and SDK. The current production assembly
contains several changes beyond macro expansion of `LzFindOpt.asm`:

- The SDK keeps the cut counter on the stack and maximum length in `r12`;
  the product keeps the cut counter in `r12d` and maximum length on the stack.
- SDK forms a child address from the saved delta and current input pointer;
  the product keeps an invariant address base in `r8`, moves the constant
  length through `xmm3`, and compares the child with a saved parent directly.
- SDK extends a match byte by byte; the product enters bounded SSE2/word
  extension after the first two equal bytes, then uses a scalar tail.
- The product additionally rejects a zero head and checks descending-child
  validity before consuming the cut count. These preserve the checked
  kernel's invalid-table behavior and must remain in any comparison port.

The full upstream source, current assembly, System V register definitions,
wrapper margins, long-match copying and error order have now been read
together. Before optimizing Rust wrappers, compare a complete faithful
SDK assembly variant (retaining those two required checks) against the
current variant in one binary. Keep both implementations and their evidence;
do not revert only the vector scan or only the register allocation and infer
the whole kernel's value from that fragment. This comparison is outstanding;
the current assembly has not been changed by this allocation round.

### Complete SDK BT variant and native comparison

The full upstream System V macro expansion now lives beside the shipping
variant in `bt_kernel_x86_64_sdk.S`. It retains upstream register allocation,
stack cut counter, child-address reconstruction, scalar match extension,
and long-match copy loop as one unit. The only intentional semantic changes
are the existing zero-head refusal and descending-child check before cut
exhaustion. Both variants use the same A-F wrapper and integer/direct-mode
guards. No production default has changed.

The existing non-default `kernel-ab` feature selects the variant once via
`LZMA_TURBO_BT_ASM=current|sdk`; ordinary builds contain no new runtime
selector or environment read. All six native register-walk tests now invoke
both variants explicitly, without mutating process-global environment.
Debug and release suites both pass, including 60,000 generated table cases
per variant, every short-output/error-order case, and match/tail boundaries.
Miri and other architectures continue to exercise the portable path once.

The frozen x86 comparison binary is
`895e70dc9b63099a09a99247ac4df1111c26b37732c32351a0756a70d8398372`.
It uses release optimization with debug symbols for subsequent attribution.
The thirteen-case cohort compares both variants in this same binary, the
preceding production binary, and the pinned official SDK. It records run
ranges as well as three-run medians, after one warmup. An unavailable
`profiling` Cargo profile was a setup failure; the successful build uses
the existing release profile with symbols retained. Neither the setup
failure nor timing outliers are discarded from evidence.

The complete hash-producer and lz-consumer source comparison also covers all
seven `GetHeads` families, local masked CRC tables, low-hash load/store order,
`MixMatches2/3/4`, specialized skips, vtable selection, held-block ownership,
normalization, and failure parking. Their active SDK algorithms are already
ported. The commented-out SDK BT4 inline consumer and fourth low-hash table
are not active upstream optimizations to copy blindly. Remaining wrapper
costs require profile attribution and complete bounds proofs before changes.

The operator has resumed work on the ARM host; no new local benchmark or
substantial build is running. The encoder comparison uses the Linux x86
benchmark host. A separate Windows audit owns non-solid single-thread decode
RSS; it does not overlap the encoder measurement host or source edit lane.

The thirteen-case comparison completed 208 samples and 4,614 compiler-load
checks with no compiler interference detected. Every archive passed the SDK
integrity check; every unencrypted product archive was byte-identical across
the three product arms. The complete SDK walk's medians versus official SDK
are below. These are diagnostic-feature results, not a production acceptance
claim; the frozen preceding production binary remains a separate arm.

| Encode case | Wall vs SDK | CPU vs SDK | RSS vs SDK |
|---|---:|---:|---:|
| zeros L5 T16 | +1.72% | -10.68% | +0.44% |
| payload L5 T4 | +0.11% | +0.53% | -7.37% |
| payload L5 T8 | **+4.66%** | -0.27% | +0.78% |
| payload L5 T16 | +1.97% | +2.64% | +0.45% |
| payload L5 T16 AES | +1.78% | +1.88% | +0.41% |
| zeros L5 T2 | -25.03% | -9.09% | **+3.02%** |
| zeros L5 T4 | -20.36% | -11.36% | +1.39% |
| payload L5 T2 | -4.52% | -0.13% | **+3.33%** |
| tiny AES non-solid L5 T1 | -15.69% | -16.04% | **+14.31%** |
| payload L1 T16 | -10.13% | -13.15% | +1.96% |
| payload L3 T16 | -8.14% | -10.26% | +0.00% |
| zeros L1 T2 | -21.79% | -18.39% | **+9.19%** |
| zeros L3 T16 | -21.62% | -20.52% | **+14.94%** |

Against the current walk in the same binary, the SDK walk costs payload
T2 +11.52% wall/+7.76% CPU and T4 +9.18%/+5.47%. T8 wall costs 9.28%.
Plain T16 improves by 2.50% wall/2.53% CPU in this cohort, encrypted T16
by 0.53%/1.19%. Run ranges remain in the evidence. L1/L3 do not exercise
the changed BT kernel, so their arm-to-arm variation must not be attributed
to it. Tiny T1 also does not select the threaded BT walk. Its RSS/time
tradeoff remains proposed, not newly accepted by this comparison.

Separate CPU-clock profiles cover all recorded periods, including both
P-core and E-core placements. T4 total sampled CPU is 39.786 seconds for
the current walk, 41.980 for the SDK walk and 41.719 for official SDK.
The tree walk itself accounts for 19.956, 22.102 and 22.094 seconds:
2.146 of the 2.194 additional sampled seconds are inside the changed walk.
At T16, totals are 105.491, 105.353 and 105.852 seconds; tree-walk totals
are 54.832, 55.405 and 55.747 seconds. This profile does not establish an
intrinsic T16 CPU win from the SDK walk. The faithful port is retained,
production dispatch stays unchanged, and higher-level controls and the
remaining consumer/parser attribution continue. Profile timings are
diagnostics, not additional acceptance repetitions.

Sevenz evidence: `x86-sdk-bt-controls.{raw,report}.json.zst`, decoded SHA-256
respectively
`fb38bc81b69a77fb913b8076fdf02df3c15cdb8b06c813391bdc52be898d3df9`,
`abd45e2ed73df07ed878e8db0f01f97604153d5bb98195a20b0f5abdf1a55725`.
The source/generator/build/test bundle is `sdk-bt-validation.json.zst`,
decoded SHA-256
`98a21a14483a40b4aca14320cb643f43d8e925efe1e255ed4cb840162db3b4a0`.
The checkpoint privacy pass replaces its temporary generator path with a
role label; the current decoded hash is
`5b0378ecc0b70f533027b06b25acde9f1c3e0c2f23b55c88c3154cba8a26cce7`.
The evidence retains the original hashes and all measurement fields.

### Non-solid header allocation lifetime

The complete Windows T1 reader audit found a decoded header allocation of
718,570 bytes overlapping the final stream maps. Like SDK `CDbEx::FillLinks`,
the maps only need parsed metadata; the input header buffer now drops first.
The archive's retained metadata, public lookup/CRC semantics and decode
allocation count are unchanged. Plain and encoded headers without FilesInfo,
invalid maps, header terminators and output-admission ordering have explicit
regression coverage. All 64 targeted security, checksum, container and
extraction-limit tests passed on Windows.

Requested allocation peak falls from 3,257,404 to 2,865,092 bytes (-12.04%).
Five-run direct T1 medians versus the previous product change wall by +0.055%,
CPU by +0.738%, and peak working set by -3.992%; streaming changes those by
-0.025%, -1.460%, and +0.459%. Both modes pass the three-percent gate versus
official SDK on this host, with identical decoded digests, file counts and
byte counts. Windows already passed before the fix; Linux and ARM RSS gaps
are not closed by these results. Evidence is sevenz
`windows-nonsolid-rss-audit.{validation,raw,report}.json.zst`.

The second agent continues on the independent XZ buffer-lifetime/RSS audit
on the Windows host. Encoder work and subsequent Linux validation remain
on the separate Linux benchmark host.

The higher-level encoder controls also completed: another 32 samples, with
one warmup and three measured repetitions per arm at L7 and L9. Every plain
product archive remains byte-identical, and all archives pass SDK integrity.
The complete SDK walk versus official SDK is +0.15% wall/-0.55% CPU/-0.64%
RSS at L7 and +0.46%/+2.88%/+0.48% at L9. Against the current walk in the
same binary, however, it costs 16.62% wall/2.91% CPU at L7 and 10.47%/7.10%
at L9. These levels use 16/64 MiB dictionaries and 64-byte match limits;
their requested T16 does not mean the same active block count as L5. This
rules out promoting the SDK variant on requested thread count alone.

The consumer's generated code was then compared with the complete
`MatchFinderMt_GetMatches`, `MatchFinderMt2_GetMatches`, `MixMatches*`, skip
macros, block acquisition, normalization and failure-parking paths. Rust's
pair-copy loop has a much larger vectorization/bounds-check preamble, but
saved sample attribution assigns that entire copy region only 1.98% of the
current consumer's T16 sampled CPU, about 0.43% of the whole encode. T4's
copy region is about 0.32 seconds. Instruction count alone does not justify
replacing this loop as the answer to the remaining whole-path gap. Low-hash
accesses and advancement dominate the consumer samples; CPU-clock samples
can skid, so individual sampled instructions do not prove a cache or branch
cause. No unchecked consumer rewrite has been made on that assumption.

Sevenz evidence: `x86-sdk-bt-high-level-controls.{raw,report}.json.zst`,
decoded SHA-256 respectively
`38c2fc701609ef2e9462dde89570156f5f02cc6f88c5c53924c5d201e102006b`,
`c8bdfb5ffe0195a42188442ce4d1111380b217ef43e616e0079e4b26aefdd4a9`.
Full profile reports, instruction annotations, generated code and raw-recording
hashes are in `x86-sdk-bt-profile.json.zst`, decoded SHA-256
`8ef52b8d9d41070872d23ea2261da0bea99f84778068a98535940e03c0349a36`.

### XZ buffer ownership attribution on Windows

The fixed original-harness four-filter cohort and 120 diagnostic processes
separate caller storage from library storage. Each filtered decode uses one
8 MiB dictionary, a 28,544-byte probability table, 64 KiB library input and
64 KiB filter scratch with bounded carry. The caller adds 1 MiB input and
1 MiB output allocations, but allocation size is not residency: removing
only the redundant caller input buffer saves about 1 MiB resident, while
reducing output capacity saves only about 50 KiB. The filtered reader returns
roughly 64 KiB at a time and does not touch the full output allocation.

The unchanged original harness passes wall/CPU versus xz 5.8.3 on these
Windows rows, retaining about a seven-percent working-set gap. All diagnostic
CRC/byte checks pass and reader destruction releases its allocations. The
audit found no missing library port justified by this evidence; neither the
library nor the benchmark harness was changed to erase the RSS row. The
original Linux process-RSS discrepancy remains open and separate. Evidence:
`windows-xz-rss-audit.{raw,report,validation}.json.zst`. The second agent
continues immediately with the independent BCJ2 source and profile audit.

### Combined SDK-walk and header-lifetime capstone

The native x86 capstone passes strict workspace Clippy, formatting, both
workspace all-feature suites, the sevenz default AWS-LC suite, lzma no-std
and encoder-only checks, and the normal release build. Cargo was used with
`--no-fail-fast` because Nextest was unavailable. Including doctests, the
independent configurations report 412, 488 and 458 passed tests respectively;
their ignored counts are three, one and zero. Two lzma memory-guard stress
tests and documentation examples remain ignored as declared by the suite.
This is not a Miri, ARM, wasm-conformance or external SDK-assembler run.
The C SDK oracle was required. All 121 production source-file hashes match
the local snapshot before the independent normalization work begins.

Normal production binary:
`5f258072e238c9f683ca5e8e676b26750cf38d130d7ba5d76bd57a0770deb4e2`.
Sevenz evidence: `x86-sdk-header-capstone.validation.json.zst`, decoded
SHA-256 `363162bad5f1415e56d76993d7c3e236476aeaa4a0da10dc5dad1a8d11349914`.

### BCJ2 Windows source and scheduling audit

The complete BCJ2 graph audit confirms the requested-input refill, partial
word fallback, owned 256 KiB transfer, positional/ring input, final checks
and cancellation path. The SDK binder still copies into its consumer.
Neither the source comparison nor profiling justifies an independent-binder
rewrite; no source changes were made in this audit.

Seven measured repetitions at T2/T4/T8 are 4.3–4.8% faster in wall time
and use 6.6–10% less peak working set than the fixed SDK T1 mixer pairing.
Windows process CPU-time accounting undercounts this workload in both the
managed and native APIs, particularly for the SDK. Those CPU seconds cannot
close or establish a regression. Independent T4 process-cycle medians are
2,659,314,564 for the product and 2,769,188,148 for the SDK. Eight scheduling
traces per arm give about 0.782 versus 0.811 seconds scheduled CPU, with
8,277 matched intervals, no unmatched pairs and no lost events. These are
diagnostic cross-checks, not substitutes for the acceptance CPU metric.
The PDB lacks Rust function names, so there is no function-level attribution.
All seven product digest checks and SDK integrity checks pass.

The Linux T4 CPU discrepancy remains open. Sevenz evidence:
`windows-bcj2-consumer-audit.{raw,report,validation}.json.zst`; decoded
SHA-256 respectively
`0d0af4e519426bfcbf5600239c5356c7e5cb38f895d12e6d7f37bbcc48b7fbeb`,
`266f3bcfab817e6b2021266182b0f133fbf2bf541276dc3db196c4ee9c602a4d`,
`9700ba1b908224f850f59971e582dc22062065831c4ecea6a7d2b9e21042d07c`.
The second agent proceeds to the SDK normalization kernel family, a distinct
long-stream path; it does not explain the current short-block encoder gap.

### Linux header residency and current decoder controls

The five-measured-repetition header cohort retains correctness and timing,
but does not close RSS. Direct T1 is +0.17% wall/+0.17% CPU/+9.45% RSS
against the SDK; streaming T1 is -1.06%/-1.05%/+16.77%. Against the frozen
preceding product, RSS is +1.30% direct and +5.20% streaming. All four
product digest checks match, all 36 samples succeed and no compiler
interference is detected. An initial preflight stopped before measuring:
its expected hash belonged to a different fixture. The existing x86 archive
was verified by hash and decoded contents before the corrected cohort.

Mapping traces show identical heap growth in both binaries: 2,314,240 bytes
direct and 2,625,536 bytes streaming, with the same anonymous mapping sizes.
Three additional diagnostic repetitions per arm/mode show anonymous RSS
within one page, while file-backed RSS rises. At the sampled steady point,
the candidate executable's text and read-only mappings account for about
452 KiB more RSS. Generated symbol sizes differ only in header functions,
unwind records and the renamed diagnostic-walk wrapper; total executable
text grows 64 bytes. The larger resident footprint is therefore not a larger
live header heap, and an allocator-policy change is not justified by these
traces. The RSS regression remains recorded; it is not waived or claimed
fixed by the lower requested-allocation peak measured on Windows.

The subsequent seven-repetition production controls retain the BCJ2 T4
CPU miss: -1.20% wall/+3.52% CPU/-2.39% RSS versus the fixed SDK mixer.
Encrypted near-incompressible T16 passes this cohort at +0.37%/-1.91%/-8.84%.
Its previous product build passes alongside it; no changed decoder path
explains that control's movement from the prior +3.50% wall result.

Five CPU-clock and five precise-cycle profiles per arm cover current
production T2, T4 and the fixed SDK pairing. No lost samples were reported.
The whole-path comparison still assigns most work to the already-ported
LZMA assembly; the BCJ2 converter consumes about 0.058 seconds at T4 versus
0.052 for the reference in these diagnostic runs. P/E placement varies,
and profiler CPU totals are not interchangeable with uninstrumented gate
timings. A complete pointer-cursor `Bcj2Dec_Decode` port is being compared
with the existing checked decoder in one diagnostic binary. Default builds
continue selecting the checked decoder; neither a smaller copy-loop tweak
nor an unsupported synchronization rewrite has been selected.

Sevenz evidence and decoded SHA-256:

- `x86-header-lifetime-controls.raw.json.zst`:
  `29f2cc6adb9db0fbf6d6335981c748e7eff95b6bd702ba7dab4de8d38c94d7cf`.
- `x86-header-lifetime-controls.report.json.zst`:
  `937d4524e5a97ec59a5462cfcd811b78404456b360b5d165ef0d760870b22132`.
- `x86-header-residency-diagnostic.json.zst`:
  `ef3e0c9b13e611fb0ded9672ad0a912c9ff4ee2acf7abaeb82c45f01dc469c16`.
- `x86-current-remaining-decode-controls.raw.json.zst`:
  `291f2d5ef6f6b10d90867f5101e9ae06f52a63734e5c7645eb8c3de382df332f`.
- `x86-current-remaining-decode-controls.report.json.zst`:
  `ffabb6654df518fa68723f1f191b34e6ba3b6b853003ea5618204c6b3b1bd27f`.
- `x86-bcj2-production-profile.json.zst`:
  `6c750ea7a3ebcdf72fa0e462658c909c57e9e3df80d3ff3c4fe5e7e436f1d264`.

### Complete SDK normalization family

The caller audit covers single-thread hash/son tables and the threaded
high-hash, son and low-hash tables. Each finder reaches normalization near
its 32-bit position wrap; ordinary LZMA2 blocks reset first. This port cannot
explain the short-block T4/T8/T16 discrepancies. The former default x86
codegen used SSE2 compare/mask saturation. The port adds the SDK SSE4.1,
AVX2 and ARM64 NEON family, its 128-byte prefix/bulk/tail split and four
vector operations per kernel iteration. Runtime x86 feature selection occurs
once per normalization call; no-std builds use target features or scalar.

Windows native tests, 49,920 SDK differential cases, integrated encoder-only
tests and true no-std compilation pass. ARM64 compile-only verification uses
the installed 1.96.1 toolchain and emits unsigned saturating subtraction;
it is not native ARM execution. A continuous LZMA1 encoder processes
4 GiB + 32 MiB of generated input and demonstrably normalizes both hash and
son tables. All baseline, candidate and SDK streams are byte-identical at
610,675 bytes, SHA-256
`fdae006353d91dfdbafe16126801e5584fece9f499c996d0d354b2a7d12cc44a`.
The independent SDK decoder verifies all 4,328,521,728 output bytes.

Three paired end-to-end medians are 10.26144 seconds before and 10.29975
after (+0.37% wall), with identical 12,423,168-byte peak working set and
process cycles +0.34%. The SDK takes 18.90778 seconds at 12,132,352 bytes;
the candidate is 2.40% above its working set. The large existing timing
advantage is not attributed to normalization. Isolated kernel medians improve
65.7%, 60.4% and 20.5% for 256 KiB, 4 MiB and 256 MiB tables, within 0.4%
of SDK wall time. CPU measurements and the Windows accounting caveat remain
in the evidence. The faithful family is retained without claiming a whole
encoder speedup where it was not measured.

Lzma evidence: `windows-normalization-audit.{raw,report,validation}.json.zst`,
decoded SHA-256 respectively
`0ca5f2f3bc84f1fb927510ac66c394cdf74c6fbd99b0fe35dca49ff6d0e62856`,
`a4d0b81de89b8ab6fd53b16eccb766d02a456c4a47c017876c3e5b5a98105557`,
`a275223fb1386d45144e0821ed58964e174daaac4b4b1c013e80f6e487b74ded`.

### Complete BCJ2 cursor port: correctness and independent Windows controls

The diagnostic cursor implementation ports the complete `Bcj2Dec_Decode`
state machine: priming, pending output, main scan, range normalization,
probability updates, side targets and cursor writeback. It is retained behind
`kernel-ab`; default production decoding still selects the checked slices.
Native differential tests compare every state field, consumed input, output
and errors across split streams, short output and malformed input. Both
tests pass under Miri with `filters` enabled. The first Miri invocation
selected zero tests without that feature and is retained as an unsuccessful
validation attempt, not counted as coverage.

Targeted native validation passes twelve BCJ2 unit tests, five required C SDK
oracle tests, nine container bridge tests and eleven archive tests, plus
no-std filters and encoder-only compilation. Normalization passes its native
tests and actual Miri scalar-wrapper test. The first diagnostic build used
an invalid package/feature selection; selecting both the library and bench
packages succeeds without manifest changes. All 123 source-file hashes
match the diagnostic snapshot. This batch has not yet had its full capstone.
Sevenz evidence: `x86-bcj2-cursor.validation.json.zst`, decoded SHA-256
`3e573d87bfb3555323c780107f17a53bb38a100eaa591d5d27cbfdb3bbbef893`.

Independent Windows checked/cursor/cursor/checked cohorts retain six
measured T1 repetitions per mode: cursor wall changes -0.41%, with essentially
unchanged peak working set. Five-repetition same-binary T2/T4/T8 controls
change wall +0.53%/-0.76%/+0.54%, cycles -0.002%/-0.92%/+0.15% and working
set less than 0.06%. Windows CPU accounting remains unreliable for the
ordinary SDK mixer and does not establish acceptance. All 124 original and
parallel records succeed; fourteen digest records agree. These results do
not justify selecting the cursor port or closing the Linux CPU discrepancy.
Sevenz evidence: `windows-bcj2-cursor-audit.{raw,report,validation}.json.zst`,
decoded SHA-256 respectively
`5c6d17473a7890e277b0e3f646dd26dd66b15dd3afb1240c988ebf198a6e708a`,
`aa4f0731b2ab5cf65db920e2468834d4f8d107eada597690edd22d82a48612ed`,
`4d65829a4a0d42d6b21ca0766474f370444f44699783c61c76d86104758b9a29`.

The Linux matrix completes 320 samples: one warmup plus seven rotated
measurements for four arms over ten cases. All thirty preflight product
digest checks agree, every SDK integrity check succeeds and no compiler
interference is observed. Single-folder T1 uses `-mmtf=off` for the literal
one-thread reference; higher single-folder budgets retain the fixed SDK
mixer pairing. Multi-folder controls use the requested budget on both sides.

| Cursor case | Wall / CPU / RSS vs checked, same binary | Wall / CPU / RSS vs SDK |
|---|---|---|
| Single T1 | +0.26% / +0.25% / +0.25% | -3.70% / -3.69% / -6.63% |
| Single T2 | -1.19% / -2.50% / -0.29% | -3.11% / -2.10% / -5.97% |
| Single T4 | -0.53% / +0.59% / +0.06% | -2.52% / +3.20% / -2.53% |
| Single T8 | +1.87% / +1.00% / -0.29% | -0.51% / +4.08% / -2.71% |
| Single T16 | -0.56% / -0.95% / 0.00% | -1.67% / +3.08% / -2.25% |
| Single T2 streaming | -0.91% / -1.51% / -0.15% | -4.19% / -2.94% / -5.96% |
| Multi T2 | +0.54% / +0.55% / +0.35% | +2.30% / -2.05% / -6.97% |
| Multi T4 | -1.32% / +0.22% / -0.10% | -3.44% / -0.43% / -4.46% |
| Multi T8 | +1.24% / +0.43% / -0.07% | -5.20% / -0.45% / -2.38% |
| Multi T16 | +0.98% / -0.70% / -0.11% | -8.78% / -1.83% / -3.60% |

The cursor is not selected for production. Five CPU-clock and five precise
cycle profiles per arm explain why a whole-path win is limited: converter
sampled cycles decrease from about 223.3 million to 212.2 million, while
LZMA decoding accounts for about 2.73 billion cycles in either arm. Whole
process sampled cycles change about -0.26%; the reference converter is
about 199.2 million. Instrumented CPU-clock totals differ from uninstrumented
timings and do not supersede the matrix. The faithful cursor port remains
available for the wider work; no speculative synchronization or buffer-size
change follows from these profiles.

Sevenz evidence and decoded SHA-256:

- `x86-bcj2-cursor-controls.raw.json.zst`:
  `cad2d2a81946d2de2be73dc01adab49ccd5c94fec0caea0f21c554e23553acc5`.
- `x86-bcj2-cursor-controls.report.json.zst`:
  `4cbd27482628e62eb8dbb658b2772243bade330caa35f57aacf6939f4f30b8bf`.
- `x86-bcj2-cursor-profile.json.zst`:
  `14f1fcae98a29cf4ccc6cbfc28ca10996d7d10c4bf70ef9fc332aff258b6699e`.

### Threaded normalization and encoder position-wrap audit

The full normalization caller audit found two related overflow-check defects.
Hash input read-ahead can wrap `stream_pos` before `pos` reaches its
normalization threshold; rebasing that position used ordinary subtraction.
The SDK's `MatchFinder_REDUCE_OFFSETS` uses unsigned modulo arithmetic.
An inline boundary test fails before the correction and passes afterward,
checking available input, queued distances and untouched low-hash/tree
ownership. The hash position now uses explicit wrapping subtraction.

The same audit followed `LzmaEnc_CodeOneBlock` into all `GetOptimum` position
uses. The coding position increment, block progress differences, parser
increment and look-ahead sums also require UInt32 wrap. They now make that
arithmetic explicit while retaining the full 64-bit processed count. Masked
position states and bounded first-byte increments remain ordinary arithmetic;
the change does not replace bounds checks with wrapping indiscriminately.
The new regression shifts only coding positions while retaining their low
context bits. Fast HC4, inline BT4 and threaded BT4 compare byte-identical
output and exact progress across both final-block and 128 KiB return paths,
using checked and unchecked parsers. It panics at the coding-position
increment before the fix and passes afterward with overflow checks enabled.

Actual threaded streams provide independent coverage beyond the synthetic
boundary test. A generated 4096-byte xorshift tile is repeated for 32 MiB and
4 GiB + 32 MiB, with a 1 MiB dictionary, normal BT4, two requested finder
threads and an end marker. Linux SDK C, normal Rust and overflow-checked
Rust produce identical outputs for both sizes. The long stream is 614,822
bytes, SHA-256
`9cf0d006a14052789493c762d694aa1bc2e0c460b38de45fda8ad0e5f60f1210`.
The Windows overflow-checked run first reproduces the coding-position panic,
then produces the same bytes with the complete fix. Independent SDK decoding
on both hosts verifies every generated byte, all 4,328,521,728 output bytes
and complete compressed consumption. No generated archive is committed.
Windows measurements stop when the host is assigned other work; these are
correctness results, not new timing acceptance claims.

Lzma evidence and decoded SHA-256:

- `x86-mt-wrap.validation.json.zst`:
  `3c686d4b50cd526a9468c19a68f23d15abf9bb6925412d3f64986338a536110a`.
- `windows-mt-wrap-audit.raw.json.zst`:
  `3daf34eb479050ae7bdf40523190d836864e694e6a031bfbd5ba66389c119196`.
- `windows-mt-wrap-audit.report.json.zst`:
  `6cf3a90e5c3e87b87250d67d8571b212ffdaa5a0c0ba796874403555d25a4802`.

The combined normalization/cursor/wrap capstone passes formatting, strict
workspace Clippy, both all-feature workspace suites, the default AWS-LC
suite, no-std and encoder-only checks, and a normal production build.
Including doctests, lzma reports 418 passed/three ignored; sevenz all-feature
488/one; sevenz default 458/zero. All report zero failures. Cargo uses
`--no-fail-fast` because Nextest is unavailable. The all-feature suites
select the diagnostic cursor; ordinary builds retain checked decoding.
All 123 production source-file hashes match the local snapshot. ARM native,
wasm-conformance, Valgrind and external SDK assembly validation are outside
this capstone; earlier targeted Miri results remain separate.
Normal production SHA-256:
`9773eb7bdd0d11260717d59e3f92c0b97bc0d4ff6ff50433add6fc7d04086378`.
Sevenz evidence: `x86-kernel-wrap-capstone.validation.json.zst`, decoded
SHA-256 `981608b7492ec9f0d787eb2cae448b04cf4e124687138db1e3876ae44ae77b79`.

### Complete low-hash mixer audit before precise profiling

All active SDK combinations are present: BT2 without mixing; BT3 with
`MixMatches2`/Skip2; BT4 with `MixMatches3`/Skip3; BT5 with `MixMatches4`/Skip3.
The fourth hash table and specialized BT4 consumer remain disabled upstream.
The audit covers get/skip transitions, short tails, nearest-distance cutoff,
early-return order, low-hash ownership and normalization, held-window
lifetime, long-match extension, trusted margins and producer failure.
There is no missing active algorithm to port in this family.

An extracted direct SDK differential passes 1,800 cases, comparing complete
tables, output and sentinels across all active mixers, expired/near heads,
low alphabets, nonzero output offsets and positions near UInt32 wrap.
Generated trusted kernels contain no bounds/panic calls and retain both
independent head loads before either store. Standalone ABI and instruction
differences are not evidence of a throughput gain.

The earlier mixer attribution includes the containing consumer function.
In the saved T16 SDK-walk profile, that function accounts for 22.882 of
105.491 sampled CPU seconds; the SDK mixer plus consumer account for
20.609 + 1.164 seconds. Queue copying remains about 0.43% of whole-process
sampled CPU. CPU-clock samples landing at stores and state advancement can
skid; they do not prove those instructions caused the stalls. The next
attribution therefore uses precise load latency and branch misses across
the complete consumer, keeping P-core and E-core events separate.
No source change is selected by this audit alone.

Lzma evidence: `low-hash-mixer-audit.{raw,report}.json.zst`, decoded SHA-256
respectively
`476ca96a6899204b5b06346ff97b6317d8c7855257f53de86f3a22f659cb4c52`,
`0a0def2d5e583fab6482396d353d5fe6f5ee7c5161f2a75e94902a44b603c1c6`.

### Long threaded stream resource diagnostic

The completed wrap-path cohort compares the normal library probe with the
pinned SDK C oracle, using one warmup and three rotated measured repetitions
per size. Every output digest matches and no compiler interference is
observed. At 4 GiB + 32 MiB, median wall is 20.121 versus 24.926 seconds
(-19.28%), CPU is 32.684 versus 39.411 seconds (-17.07%), and peak RSS is
22,855,680 versus 21,876,736 bytes (+4.47%, 0.93 MiB). This is a proposed
memory/time tradeoff for operator review, not a passed 3% RSS gate.
The 32 MiB row is -28.95% wall, -22.90% CPU and +4.15% RSS; its short
durations have substantial spread. Neither row isolates normalization's
effect: this cohort has no previous scalar implementation arm, and the
reference is the SDK C probe rather than the fleet's official executable.

Lzma evidence: `x86-mt-wrap-timings.{raw,report}.json.zst`, decoded SHA-256
respectively
`a3345ca0044f271dcadd08e0f99e72b93ed18bd81ce144fc5f96da80c0eaf7dc`,
`37eb5c3fc7e13338b5f4ae8bc5c074fc1457b814f2e258e21bfbde5720b4d87d`.

### Complete range-encoder family audit

The audit covers carry/cache propagation, 64 KiB flushes, deferred first
write errors, processed counts, base and direct bits, literal and matched
literal trees, reverse trees, length coding, rep selection, position slots,
alignment and end markers against the pinned SDK. A 1.2-million-operation
differential compares every intermediate state and final output, including
4,976 pending-carry observations and first/later write failures. All pass.
The active SDK has no missing architecture-specific range kernel.

Integrated release assembly confirms a remaining port difference: general
length coding and known zero/one bits still store or reload range around
individual operations, whereas the SDK retains range across coding groups.
Literal coding already retains it. This supports a complete port of the
remaining SDK state boundaries; it is not a performance result. The port
must retain probability updates, flush/error behavior and state writebacks
at the reference boundaries before its timing is evaluated.

Lzma evidence: `range-encoder-family-audit.{raw,report}.json.zst`, decoded
SHA-256 respectively
`bc5f70840435bcbd94ed50126a22d92f2e8729ff622f2132ff569552fa29381f`,
`51b0964d7d73140f63697c5ed38a2df9c7502568a77824447a137ccd05408b8c`.

### Precise consumer attribution and complete range-state port

The T4/T16 consumer investigation records three rotated repetitions of
precise load-latency and branch-miss events for the frozen product and SDK.
All 24 archives pass SDK integrity checks, all recordings report zero lost
samples, and P-core/E-core PMUs remain separate. Raw recording hashes,
decoded samples, symbol/placement reports, assembly and drivers are retained
in `x86-consumer-precise-profile.json.zst` in sevenz evidence. Its decoded
SHA-256 is
`9bbb1827ec99a9f2bf0d5ad55e8f0e88e19ba0b27d8d78b0c6800659921d9da0`.

The dominant consumer load sites are the three-byte hash head and its input
history verification, in both implementations. On P cores, median consumer
shares of the sampled load-event periods are 9.68% product/9.25% SDK at T4
and 15.27%/15.97% at T16. Sampled load latency rises at full fan-out for both
implementations. These periods are event estimates, not elapsed time or
isolated stall costs; neither PMU counts nor its latency units are summed
across core types. Branch samples likewise concentrate on the same history
match decisions. This does not establish a missing mixer kernel or justify
an unchecked queue-copy rewrite. The source audit also confirms isolated
writer state and exclusive low/high hash ownership are already present.

The separate range-family audit leads to a complete SDK range-local port:
known/general bits, length/reverse trees, rep selection, position slots,
direct/alignment bits and end markers now retain range at the reference
boundaries. Carry/flush/error handling and probability updates are preserved.
The SDK state oracle again passes all 1.2 million operations. A new checked
regression covers every length and position state, reverse-tree widths,
output-buffer crossings and first/later write failures. Integrated assembly
confirms range remains live through ShiftLow rather than being stored after
each bit. Linux passes 106 encoder unit tests, encoder integrations and
encoder-only compilation. The unprofiled multi-setting resource cohort is
pending; there is no speedup claim from this port yet.

Lzma evidence: `range-localization-port.{raw,report}.json.zst`, decoded
SHA-256 respectively
`177611ac269fc13226ad7bf866d926d7336d6e6238c97eab2dd2cb4d9a0ca8ec`,
`4e5882e7697d9b9829e16ad50a904d9faf1ae38d08beccdaf75bd6b1a7a6ed56`.

### Range-local resource cohort and next attribution

All 156 samples across 13 settings finish with successful SDK integrity
checks, no detected compiler interference, and identical plain archive bytes
between the preceding and range-local product builds. Each arm has one
warmup and three rotated measured repetitions. Reference ratios below are
percentage changes in wall time, process CPU and peak RSS; negative is lower.

| Setting | Wall | CPU | RSS |
| --- | ---: | ---: | ---: |
| Payload L5 T2 | -16.82% | -10.96% | +3.38% |
| Payload L5 T4 | -2.31% | +0.81% | +1.29% |
| Payload L5 T8 | -5.17% | -2.50% | +0.50% |
| Payload L5 T16 | +4.30% | +4.03% | +0.53% |
| Payload L5 T16 AES | +5.00% | +5.40% | +0.22% |
| Zeros L5 T2 | -18.31% | -8.29% | +2.94% |
| Zeros L5 T4 | -27.77% | -16.03% | +1.25% |
| Zeros L5 T16 | +0.39% | -4.48% | +0.44% |
| Tiny files L5 T1 AES non-solid | -15.44% | -15.89% | +14.44% |
| Payload L1 T16 | -15.50% | -15.23% | +1.94% |
| Payload L3 T16 | -7.21% | -10.69% | -0.44% |
| Payload L7 T4 | -3.80% | +0.47% | +0.70% |
| Payload L9 T4 | -11.23% | -7.41% | +0.50% |

This does not close the full-thread timing or T2 memory gaps. Tiny encrypted
files remain a proposed memory/time tradeoff. Against the preceding product,
L7 and L9 CPU decrease 3.80% and 3.39%, but L5 T4 wall/CPU increase
5.48%/5.21% and zeros T2 increase 4.40%/4.06%. Those regressions are not
waived by passing reference ratios. T4 RSS has two modes in both old and new
builds; the source port changes no allocation. A resident-page snapshot
shows huge-page-backed major buffers but does not establish why slot
residency differs. Keep the faithful port and attribute complete-path CPU
and instruction-fetch behavior before another implementation decision.

Sevenz evidence, decoded SHA-256:

- `x86-range-local.validation.json.zst`:
  `123490b5a1038636ede4fed2784990da024ca58050e846a1ec1420635d7e5197`.
- `x86-range-local-controls.raw.json.zst`:
  `34a2a27c1877dc6f63abbdf0933611a0e022ac745f8bed32b4ba03ade8bff490`.
- `x86-range-local-controls.report.json.zst`:
  `64fd16cb9a0ace8c8ed1a94993d90dd885b6a9e2928b04496c87b879f7792dd1`.

### Parser, price lifecycle and ARM position slots

The complete normal-parser audit covers initial and compound candidates,
strict tie rules, horizon handling, state/repetition reconstruction and
Backward. The price audit covers initialization, the active 64-match and
64-long-rep refresh counters, lookahead-drain boundaries and LZMA2 stored
fallback. No missing active parser decision or refresh schedule is found.
Stripped SDK regions are identified from complete instruction sequences:
the dominant literal branch is RC normalization, other sampled regions
are match-price replacement, pair-boundary gating and repeat extension.
These are shared algorithmic decisions, not evidence for deleting checks.

One architecture omission is established: ARM and AArch64 should use the
SDK's CLZ position-slot family rather than allocate and initialize the
16 KiB fast-position table. All three helpers, constructor storage and
conditional dictionary guard are ported together; non-ARM behavior is
unchanged. The independent arithmetic differential passes 1,114,206 cases,
including all small positions, bit transitions and randomized full-width
positions. Compiled native/Miri tests, ARM cross-compilation and eventual
native ARM timing remain separate pending checks. The deterministic table
removal is not a measured process-RSS claim.

Lzma evidence, decoded SHA-256:

- `price-lifecycle-audit.report.json.zst`:
  `8dbd928ef72d7e300154ba4f79d5f18112049df4dbc8b46eab59b7d40fb42e0f`.
- `normal-optimum-parser-audit.raw.json.zst`:
  `dd4e81bb6bcfa4bfefb7220e76a9ca6757c42da602908af579ab5a52baf6bda3`.
- `normal-optimum-parser-audit.report.json.zst`:
  `9ded070f443baffb5884250000ee9737a2f9c220e2d4d1fb44c377eb7b25b2d2`.
- `sdk-parser-region-identification.raw.json.zst`:
  `3008341ab8675f81bbf99f3fb5113c8aedc906c51ac050400da030d23f0b2ff9`.
- `sdk-parser-region-identification.report.json.zst`:
  `fb670d20c93ec1d3e6fe6dcb2324d4bd569943dcb230cf06abd9375929913663`.
- `arm-position-slot-port.raw.json.zst`:
  `886bea19808d13ed8f13f0d40ead08f65a45e132693f520e4c2eee981227a565`.
- `arm-position-slot-port.report.json.zst`:
  `75630db5e7b33773f22cac742e7c809d958dbf9f398d9b3cf63a4d7f2aef628d`.

The separate streamed-input audit identifies T2 staging in the actual
archive path: an initialized 2 MiB source buffer, up to four 256 KiB input
chunks and the bounded output bridge. Existing LZMA2 pull encoding supports
a Send input with caller-local output at T2. An additive archive API can use
that path while preserving existing generic non-Send APIs, AES/output
ownership and T4 behavior. This is an implementation candidate, not measured
savings. Sevenz evidence `streamed-lzma2-input-lifecycle.report.json.zst`,
decoded SHA-256
`7c21ab2af0d60ea28fe593cefbe8ba1bfc9722d5fd0d7fed13ea61711b9d000c`.

### Post-port profiles and compiled ARM checks

The frozen range-port profile cohort completes 18 recordings: T4/T16,
preceding product/current product/SDK, three rotated repetitions. All
archives pass SDK integrity checks, all decoded sample exports succeed,
and all reports show zero lost samples. CPU-clock periods, P-core frontend
misses and E-core instruction-cache misses are recorded separately.

Mean sampled CPU at T4 is 39.121 seconds preceding product, 39.650 current
product and 40.671 SDK. The current-minus-preceding increase includes
0.276 seconds in the unchanged tree walk, 0.103 in the unchanged consumer
and 0.104 in parser/coding. It is not concentrated in the newly localized
length encoder. At T16 the totals are 104.592, 103.501 and 99.878 seconds.
Current versus SDK differences include tree walk +0.363 seconds, consumer
and mixer +1.582, and hash heads +0.482. Coding comparisons retain the
SDK's separately identified literal function and partially identified
encoder address region; unknown boundaries are not silently treated as
complete functions. These are diagnostic profiles with instrumentation
overhead, not replacement acceptance measurements.

P/E placement also changes by role: at T16, the preceding product has
15.715 sampled seconds of tree walking on P cores and 36.408 on E cores;
the current product has 18.109 and 33.590. Total process P-core shares
are similar, so aggregate placement alone hides the shift. Frontend events
have only tens to hundreds of samples per recording. Their counts cannot
establish a causal instruction-cache cost or justify an inlining change.
The faithful range port stays in the working tree while the broader input
path is addressed. Sevenz evidence `x86-range-local-profile.json.zst`,
decoded SHA-256
`6b33a0c7ca0af8c55cc0c2ca23f8770cfeda47bd94a5c046c3327d9fcff8d471`.

The ARM position-slot test now passes natively and under Miri. AArch64
encoder-only, explicit std/encoder and complete test configurations compile;
the decoder's actual no-std/assembly configuration also compiles. The
encoder feature itself implies std through CRC, so an encoder-only check
is not described as a no-std encoder. Cross-compilation found an unused
table-macro import; a conditional import fixes it, and the ARM library/test
check then completes without warnings. Release assembly generation remains
unavailable: the existing CRC dependency also builds a shared library, and
the host has no ARM cross-linker/sysroot. The failed command and complete
diagnostic are retained; it is not an ARM source failure or a native ARM
performance result. Lzma evidence `arm-position-slot-validation.json.zst`,
decoded SHA-256
`56d8e5444515cf0f19406aded4df6164a6ea4fcb6c8e566f2ae8ad9740d177eb`.

### Actual archive Send-input path

The pinned C++ wrapper audit confirms a direct destination-pointer chain
from the archive folder reader through the sequential stream wrapper to
LZMA2 and the ordinary file reader. `CFolderInStream::Read` caps each
request at 1 MiB and checksums those bytes in place; that cap is not a
staging allocation. The new additive single-entry and solid-entry Send
APIs follow this ownership path for a one-block-coder LZMA2 configuration.
Only the source is borrowed by the scoped finder. Output, AES, packed CRC,
size counters and archive bookkeeping remain on the caller. Existing
generic APIs still accept non-Send input/output, and multi-block T4 routing
keeps the preceding bridge. Filesystem compression helpers and the normal
benchmark caller use the same public APIs.

The new route retains the SDK's 1 MiB request cap before checksumming,
does not change size hints, and uses no extra input allocation. The coder
is taken out of its reuse slot while encoding and restored only on success;
errors and unwinding cannot retain partial encoder state. Reader panic
payloads resume on the caller after producer shutdown. Focused tests pass
for thread ownership, original source/output errors and panic recovery,
reuse, bytes across block boundaries, CRC/count/request caps, archive
headers/substreams, AES roundtrips, generic non-Send callers and T4/filter
fallback. Two initial test compilation mistakes are retained with their
repairs. Full capstone and resource comparisons remain separate gates.

The complete expected-size audit rules out a hint-based allocation fix for
the current 256 MiB input, 8 MiB dictionary and 32 MiB blocks. Expected size
affects the active hash mask/initialization range, not allocated hash/son
capacity; reduce-size and hard data-limit are different contracts. A known
final streamed tail smaller than the dictionary is a genuine future
SDK-parity difference, potentially changing compressed bytes. A complete
hint port would need both push and pull paths, reset-on-reuse behavior and
understated/growing-input tests. A declared size must never become a hard
read limit. No hint change is folded into this input-path batch.

Sevenz evidence, decoded SHA-256:

- `send-input-sdk-wrapper.report.json.zst`:
  `f3759692ba7fadf64c013bfbcfe024a4b7c04b74095749402d701a84fe9b10ce`.
- `expected-size-lifecycle.report.json.zst`:
  `e4bd627304c55fa666efba3f67a952a15be14a7664de97689c746b1ec5230b85`.
- `x86-send-input-focused-validation.json.zst`:
  `5c41a9ff4d990570c2579b8a4363321675e1ae76a0c82b2308e3aea033ad4425`.

The combined range/ARM/input-path capstone passes formatting, strict
all-feature workspace Clippy, complete no-fail-fast suites, default AWS-LC,
no-std/encoder-only checks and the forced-unthreaded archive tests. Counts
including doctests are lzma 420 passed/three ignored, sevenz all-feature
495/one, default AWS-LC 465/zero, and unthreaded Send API three/zero; all
have zero failures. One redundant return binding in a new test failed the
initial Clippy gate. The complete suites continued, then the test-only fix
was checked with Clippy and the targeted test rather than repeating passed
full suites. Initial and repaired logs are both retained. All 123 product
source hashes match the local snapshot. Normal production candidate SHA-256:
`1f2eb975c1fb3c6898ec5df78395bf7f7de308b9b9ab5c107c0f7a7ec3c3ab7c`.
Sevenz evidence `x86-range-input-capstone.validation.json.zst`, decoded
SHA-256 `6e1d71f618374774b5d4dc44733b6fd2883ae9fb67adc70fc95323aedb039420`.
Resource comparisons for this candidate are pending. They exercise the
additive Send APIs used by filesystem helpers; the existing generic APIs
still retain their non-Send-capable T2 bridge, so passing the new route
would not establish parity for every legacy input API.

### Exact SDK profile boundaries and remaining ARM dispatch

The pinned SDK ELF retains unwind metadata despite stripped symbols. Its
FDE bounds identify `LzmaEnc_CodeOneBlock` as `[0x5c8d0, 0x5f4cb)`;
the startup operations and calls from `LzmaEnc_Encode2` and
`LzmaEnc_CodeOneMemBlock` independently match the pinned C. The literal,
length, price, read-match and finder-accessor helpers are attributed
separately. This refines the existing recordings without another benchmark
or any change to their raw samples. Unclassified SDK sampled CPU falls to
0.156 seconds at T4 and 0.267 at T16. Rust inlines several of these helpers,
so individual helper names are not equivalent comparison units.

The T16 complete parser/coding/price/dispatch family, including the SDK's
out-of-line finder accessors, consumes approximately 20.25 sampled CPU
seconds in the range-local product versus 19.15 in SDK. The larger remaining
individual difference is the consumer/mixer, 22.679 versus 21.097 seconds.
Hash heads add 0.482 seconds and the tree walk 0.363. Neither process-wide
P-core share nor a partial parser address range is sufficient attribution.
The full `MtCoder` source comparison also confirms staggered worker startup,
ordered writes and default Linux affinity in both implementations; it does
not establish a missing affinity policy to port. Further topology controls
must be diagnostic and must not replace ordinary scheduler acceptance runs.
Sevenz evidence `x86-sdk-function-boundaries.report.json.zst`, decoded
SHA-256 `d637ab0702eab0d57fe9e6d28adc6f93a3b6afed04cfcaef1b6fd29392703f1e`.

The remaining ARM inventory found no additional active SDK encoder kernel
missing after the retained tree, normalization and position-slot ports.
It did find a separate capability hazard in the pinned CRC dependency:
its ISO-HDLC fusion dispatch can select CRC instructions from an AES tier
without consulting the detected CRC capability. An AES capability is not
itself a CRC capability guarantee. The public parameter-based digest and
one-shot APIs bypass that fusion selector and provide a supported generic
PMULL/software route. A wrapper-only fix is being validated; it changes no
dependency, feature or normal-capability hot path. No affected hardware
execution or new ARM performance result is claimed.

Lzma audit evidence, decoded SHA-256:

- `remaining-arm-encoder-inventory.report.json.zst`:
  `49ee8d5fe56eb8bbace8dc69e97bf5aa3fa9d5e336d81fac334247ce10b1bb93`.
- `crc-arm-dispatch-audit.report.json.zst`:
  `585c0ffdb006950566df70c9177fbf2ae70ea0bd01dd5b353c80f3ba27ade669`.
- `crc-arm-capability-mitigation-plan.report.json.zst`:
  `60f65d913cd13bb4974dbbf42947b3f686c18424b9b33d8e11daa6cefcd8e334`.

The independent consumer follow-up finds nearly identical T16 E-core
fractions: 36.16% for the product consumer and 36.05% for SDK. Excess sampled
CPU appears on both core groups, so it cannot be explained simply as more
consumer work running on E cores. Earlier precise load profiles used an
older product binary and are not same-binary causal evidence for the latest
CPU recordings. No additional missing mixer transformation is established.
Sevenz `t16-consumer-attribution-followup.report.json.zst`, decoded SHA-256
`afdd5f288b52b561dd0d4f3a05787c83372bf113bcd4c568e0c2969fd6a9d778`.

At T4 with 32 MiB blocks, both `MtCoder` implementations permit four slots
for two block coders. If block one finishes while block zero encodes or
writes, its worker may encode block two into a third output allocation.
Both implementations retain that slot's storage for reuse. The Rust bridge
moves the exact allocation to the caller and waits for its return before
releasing the slot; it does not allocate a duplicate output block. Caller
delivery and channel wakeup extend the synchronous write interval and can
change the likelihood of this shared schedule. The old 100 ms mapping
samples recorded highest sampled RSS, not necessarily the OS peak, and
did not record allocation identities or stacks. Output-like mappings are
therefore an attribution lead rather than proof. Sevenz
`t4-output-slot-lifecycle.report.json.zst`, decoded SHA-256
`1f15e667180fbaa73c592eaa1af929d7dc79a1713859b2dc782f104d5fef596c`.

The XZ consumer-to-library lifetime follow-up confirms all 19 audited source
hashes are unchanged. The pinned xz 5.8.3 CLI uses the MT wrapper's direct
single-thread branch without a whole-block output pool. The Rust filtered
reader retains 64 KiB plus converter carry, not overlapping old and new
dictionaries. That structural difference does not establish the cause of
the measured 26–29% Linux process RSS excess. The Windows outer-input-buffer
probe demonstrates a caller configuration effect on that platform; it does
not close Linux RSS. Allocation identities and resident code/allocator pages
remain unresolved, so no product change or closure is claimed. Lzma evidence
`xz-consumer-lifetime-followup.report.json.zst`, decoded SHA-256
`d1dfd1b147b4548f041bc3adb113660dbe65267affc20fb8b432fc88523cc8a6`.

### Complete single-block Send-input comparison

The twelve-case cohort completed 144 executions: one warmup and three
interleaved measured repetitions for each of the previous range-local
product, single-block Send-input product and pinned SDK. All archives pass
SDK integrity checks; every unencrypted product archive is byte-identical
to the preceding product. There were no detected compiler processes during
the cohort. That check does not establish an otherwise load-free machine.
Product binary hashes and complete production source hashes are retained
in the raw evidence. Candidate hash is the `1f2eb975…` capstone above;
baseline is `a0aa688f…`, and SDK is `3d52c92d…`.

Values below are candidate percent above SDK; negative means lower.

| Input / level / threads | Wall | CPU | Peak RSS |
| --- | ---: | ---: | ---: |
| Payload L5 T2 | -17.77% | -13.28% | +0.09% |
| Payload L5 T2 AES | -18.61% | -14.43% | -1.17% |
| Zeros L5 T2 | -25.68% | -17.15% | -0.17% |
| Payload L5 T4 | -11.56% | -8.93% | +10.87% |
| Payload L5 T8 | -2.70% | -0.46% | -0.49% |
| Payload L5 T16 | +5.20% | +3.81% | +0.28% |
| Payload L5 T16 AES | +0.95% | +2.90% | +0.26% |
| Tiny non-solid L5 T1 AES | -18.07% | -18.51% | +15.29% |
| Payload L1 T2 | -15.45% | -15.40% | +6.69% |
| Payload L3 T2 | -22.65% | -22.51% | +14.58% |
| Payload L7 T2 | -13.32% | -8.61% | +0.02% |
| Payload L9 T2 | -11.55% | -8.28% | +0.004% |

Only the normal-finder single-block route changes in this build. Full-thread
passes or misses are controls, not causal improvements from that port. T4
contains approximately 300 and 328 MiB product runs; L3 T2 contains 116 and
131 MiB runs. Neither the favorable mode nor the favorable SDK comparison
is selected to declare closure. L3 T2 RSS also rises 11.54% against the
preceding product in this cohort. Timing wins with excess RSS remain
proposed tradeoffs for operator review. Earlier samples remain intact.

Sevenz evidence, decoded SHA-256:

- `send-input-controls.raw.json.zst`:
  `7df4b20ea5528ca63f74e85052d8908d5424e9c480d4bb10d8b94677b5b299eb`.
- `send-input-controls.report.json.zst`:
  `ce4cc3d70a6f3716eafc9b3549275afd5e4843234c809afc00cfd3b85c6d9de6`.

The follow-on complete multi-block port removes the archive input staging
queue for fast and normal block workers while retaining caller-local output,
exact output-allocation return and the existing refused-thread fallback.
The failure-path review additionally frees a reused coder before allocating
that fallback. This batch has no performance claim until its complete
validation and width/level/input matrix finish. Design, implementation and
failure-review evidence respectively:

- `send-multiblock-direct-input-design.report.json.zst`:
  `9f8e361853356a5aa102cfd9d1fb8ceb8bdb400dc38dccbf09956f18a18165d7`.
- `send-multiblock-direct-input-implementation.report.json.zst`:
  `b79cc05e721503c6357c38deea048a91a189f3ca83061039a67008c9251a3413`.
- `send-multiblock-failure-review.report.json.zst`:
  `4d2fb144086250de32de2cbcb936967eb4f8b1cd3cfc8850ad13179e1a5319d7`.

The complete CRC/multi-block capstone passes: lzma all-feature 423 tests,
sevenz all-feature 504, default AWS-LC 474, and forced-unthreaded Send APIs
five; zero failures, respectively three, one, zero and zero ignored. Both
format checks and strict all-target/all-feature Clippy pass, as do decoder
no-std and encoder-only checks. Initial Clippy found one equivalent
`iter().any()` assertion to replace with `contains()` in a test. All full
suites ran to completion before that test-only repair; their successful
results remain valid, and Clippy was repaired in the combined capstone.
All 123 production source hashes match the validated local snapshot.
Production candidate SHA-256:
`1b85383dd3f27cfdda60d5b6df87efc475ba34d8ee46cb3363a83178dce563f1`.
Sevenz `x86-crc-multiblock-capstone.validation.json.zst`, decoded SHA-256
`e8eef0062dc1294feae1ac6fb1670741b6e80180a5d3ba19ab0f52179227dd2d`.
Its sixteen-case matrix and clean T8 replacement are complete: thirteen
cases pass all three SDK median limits. Ordinary T16 CPU, encrypted T16
wall/CPU and encrypted tiny-file RSS miss. Zeros T16 also regresses CPU
against the preceding product despite passing the SDK. The original T8
case overlaps a binary transfer and is retained but excluded; its complete
replacement passes. Full-path CPU and physical-residency diagnostics follow
the frozen matrix before another implementation pass.

### BCJ2 complete-path attribution follow-up

The full path inventory covers main/call/jump/range input leaves, LZMA
stages, owned handoff buffers, refills, checked and cursor converters,
final checksums and output. The linked decoder kernel is accounted for;
dictionary-to-pipeline ownership still differs. The SDK extraction handler
defaults to its multithreaded mixer independently of the codec's `-mmt`
setting (`mtf` controls mixer selection). Its `CDecoder::Code` path lends
dictionary output to the binder, while our LZMA1 `LzmaReader` uses
`DecodeToBuf` semantics and copies into an owned pipeline chunk. The
`take_chunk` handoff removes a downstream copy, not that dictionary copy.
The SDK's `CStreamBinder::Read` still copies the borrowed bytes into
`CBaseDecoder::ReadInStream`'s BCJ2 input buffer. Consequently the ordinary
SDK binder path and our complete-chunk path each have one dictionary-output
copy. Their ownership, copy thread, buffering and synchronization differ;
removing one named copy site alone does not establish a whole-path saving.
Partial-word and ordinary-read fallbacks must be counted separately.
This establishes a concrete ownership-port candidate; stage-labelled native
traces must establish the exercised route and cost before changing it. It
does not change the documented T2/T1 benchmark pairing. Sevenz
`bcj2-dictionary-lifetime.report.json.zst`, decoded SHA-256:
`6fbbaaea61513e2fe23c3e4c5d6ab1a57f239c1eebc9fe90643643ae83e1b0dd`.
The pinned ELF's `.text$LZMADECOPT` is exactly
`[0x22a8c0, 0x22bb2f)`, the entire assembly `LzmaDec_DecodeReal_3` procedure;
the earlier decoder category does not contain unrelated wrappers. In the
five-recording CPU-clock reference, mean sampled milliseconds are decoder
637.319, BCJ2 47.024, CRC 13.707, kernel 18.709, one unresolved FDE 4.002,
and other 1.301. CRC explains 72.1% of the formerly unattributed reference
user CPU category. These events are kept separate from precise-cycle
recordings. Shared LZMA symbols still do not identify main versus call versus
jump work; that requires stage identity and byte/refill/wait instrumentation,
not guessing from thread creation order.

Sevenz evidence, decoded SHA-256:

- `bcj2-full-path-followup.report.json.zst`:
  `17fea5a10b66558e855e9284960198df7f9980f817f87e51ddf563edfbd27cd5`.
- `bcj2-sdk-boundaries.report.json.zst`:
  `26b4a861b7e5129a947e230a10cfabd208bf291af48a146e1c6e83d419c7de72`.

The next complete encoder profiles retain the preceding product, current
multi-block product and SDK for ordinary T4/T16, encrypted T16 and zeros
T16, followed by homogeneous-core T16 controls. CPU-clock samples, process
CPU, wall time and RSS remain distinct; core masks are diagnostic only.
Exact SDK kernel ranges now include `AesCbc_Encode_HW`
`[0x4e550, 0x4e5e5)`, `Sha256_UpdateBlocks_HW`
`[0x6abd0, 0x6aefa)` and `CrcUpdateT12` `[0x4ce10, 0x4cff2)`.
These assembly functions have no unwind FDE; complete upstream procedure,
dispatch entry and final return establish the bounds. Wrapper work remains
unattributed until proven. SHA samples alone do not identify KDF callers,
and CRC samples alone do not distinguish input, output and header checksums.
Sevenz `x86-sdk-crypto-boundaries.report.json.zst`, decoded SHA-256:
`f2263ecf04ead21d1bed8597b78ebc5f068da4fe6541e4402477bdfb6477e5b5`.

The linked current x86 decoder and pinned SDK kernel both occupy 4,719
bytes and have 64-byte section and entry alignment. After removing NOPs and
expressing branch destinations relative to each entry, all 1,408 remaining
instructions and operands match in order. The differing raw bytes are
instruction encodings and padding; this establishes no missing decoder
procedure or entry-alignment fix for this pair. It does not establish
performance parity for the surrounding reader, pipeline or output path.
Sevenz `bcj2-linked-decoder-kernel.report.json.zst`, decoded SHA-256:
`e146242f926d80f2838daa741910f07bc37e595cb93ffa27cef9736e7162789f`.

### Remaining residency gaps after the remote T16 controls

The current encoder ownership audit covers archive input and checksums,
property/thread planning, worker admission and input exchange, finder storage,
compressed output slots, caller-local delivery, AES, header construction,
reuse, errors and the memory API. No additional full-block copy or owner was
found on the accepted Send route. SDK and product both permit legitimate
schedules that first-touch extra output slots. The historical T4 and L3 T2
RSS modes therefore remain open to physical attribution; a favorable median
does not establish their cause or justify a slot cap.

The 124-file encoder source is frozen separately from subsequent decoder
work. Its current-versus-preceding fixed-product source difference is only
`lz_find_mt.rs`, the shared cursor. A predeclared 105-operation control cohort
covers L5 T2/T4/T8 and L1/L3 T2, with one warmup and every permutation of
current product, preceding product and official SDK. It retains full output
verification and all wall/CPU/RSS samples. No result is claimed while it runs.

Independent review of the previously prepared allocation diagnostic found
incomplete snapshot coverage, address-generation ambiguity, retroactive
extent labels, incomplete slot/Vec joins and insufficient terminal identity
checks. That package is preserved unchanged. A separate corrected package
must pass adversarial fixtures and native validation before its residency
reports can support attribution. An untouched SDK control was built from the
pinned source with upstream BT, decoder and AES assembly; it remains separate
from the official acceptance executable. Instrumented timings are diagnostic.

The parallel non-solid T1 decoder audit identified a missing direct dictionary
delivery path. Its candidate preserves complete Read chunk boundaries, CRC
checks before delivery, source/output error distinction and existing filters.
A dictionary-wrap request declines before consumption and keeps the copying
fallback. This is an implementation candidate, not a measured RSS closure.
The later retained x86 header control was +9.45% direct and +16.77% streaming
RSS; the older +18.92% graph cohort is not the latest baseline.

Sevenz evidence, decoded SHA-256:

- `encoder-residency-current-source-audit.report.json.zst`:
  `f13bd31afa026f7bc26e5f97da4c5aebdf587fb3dc843420a7a4aed25d809fcc`.
- `nonsolid-t1-direct-delivery-audit.report.json.zst`:
  `96b7be57f1d579735d9c1e722bf2e940f4aaf913c26b0998c66afaa59749a1d3`.

Mac timing remains deferred. Miri remains reserved for the final capstone.

### BCJ2 closure-ledger reconciliation

The top-level inventory previously carried the older T4 CPU +3.52% result
while the later 320-sample cursor comparison already contained a separate
ordinary-production arm. Recomputing all ten production/reference rows from
the retained raw samples confirms seven measured repetitions per arm and
exactly reproduces the saved medians. Production T2, streaming T2, T4 and T8
pass wall, CPU and RSS. The remaining marginal cells are single-folder T16
CPU +3.16% and multi-folder T2 wall +3.10%; they belong in final verification,
not a claim that the agreed T2 pairing is still an unresolved optimization.
The earlier phrase "single-run" describes archive decode layout, not one
benchmark repetition. Historical cohorts and diagnostic cursor results remain
unchanged and separate from production acceptance.

This reconciliation is not a new measurement or current-fleet closure.
Sevenz `bcj2-production-closure-reconciliation.report.json.zst`, decoded
SHA-256:
`d98459304b7727d4ac4b38c9a500027e4bf3498e509ea8e145ba0916df945d17`.

## Current encoder intermediate-width controls

The completed fixed cohort contains five cases, three frozen binaries, one
warmup and all six arm permutations: 105 operations. Every archive passed
official SDK integrity and decoded SHA-256 verification. All 124 production
source hashes match the frozen source inventory, and all 6,982 compiler-load
checks were clear. No samples or residency modes were discarded.

| Case | Wall vs SDK | CPU vs SDK | RSS vs SDK |
| --- | ---: | ---: | ---: |
| L5 T2 | -6.29% | -2.86% | +0.70% |
| L5 T4 | +0.20% | +0.30% | +0.32% |
| L5 T8 | +3.56% | -0.34% | +2.46% |
| L1 T2 | -13.40% | -13.13% | +2.08% |
| L3 T2 | -23.98% | -24.10% | +7.88% |

T8 wall time and L3 RSS remain open. The L3 timing/RSS tradeoff is recorded
for operator consideration; a passing low-residency sample is not a stable
memory improvement. Both product and SDK exhibit distinct RSS modes at T4
and L3. Allocation lifetime and physical-page attribution is the next step,
using isolated diagnostic copies of the exact product and pinned SDK source.
Diagnostic timings cannot establish acceptance.

Current and preceding product archives are identical for each case. This is
not SDK archive identity: L5 T2 differs by 199,585 bytes (about 0.085%), while
the other cases differ by 11 bytes. Reference settings and output sizes are
retained in the raw evidence. Preceding-product comparisons and complete
distributions remain in the report.

Sevenz evidence (decoded SHA-256):

- `encoder-residency-current-controls.raw.json.zst`:
  `d343e6f31838cfd79f2b465dfae28dc25879286059dff74d57d9bba6c5345541`.
- `encoder-residency-current-controls.report.json.zst`:
  `8e0de666cb7c2333e78e8878f8460d94c4338e4b5f28e2de9afe9f7ad9317b03`.

### Allocation diagnostic native qualification and continuation

Eight source-matched native qualification operations passed SDK integrity,
decoded SHA-256 and untouched-archive identity checks. Four instrumented
captures covered single-coder T2 and multi-block T4 paths in the product and
source-built SDK, with complete terminal identity and no lost events. These
qualify the instrumentation; they are not acceptance measurements.

The fixed allocation cohort stopped after its sixteenth operation because
the analyzer required allocator evidence for a valid 16-byte terminal output
buffer. An exact-multiple stream can acquire a fresh slot for its final empty
read. The logger intentionally records allocations only from 64 KiB upward.
The corrected analyzer checks the small buffer's observed ownership and exact
Vec return without inventing an allocation generation or resident-page proof.
Replaying the preserved trace validates all nine deliveries and leaves all
14 snapshot reports unchanged. No product allocation policy changed.

The original failed report, archive and 16 timing records remain intact. A
separate continuation revalidates and copies that prefix, then executes only
the remaining 104 operations in their original order. The pause and analyzer
revision remain explicit. The first continuation preflight also caught the
fixture's existing completion marker; its guard now permits only that exact
hash-bound marker, which the frozen benchmark source already excludes.
That preflight executed no benchmark. The continuation completed all 120
operations with correct outputs and all 36 final trace replays equal to their
saved reports. Complete-cohort attribution is still being processed. Its
first summary attempt exposed integer event-kind keys in Python versus string
keys in serialized JSON; normalizing at that boundary preserves strict report
equality. The failed summary and its diagnostics remain intact. No native
measurement was repeated for this correction.

Sevenz `encoder-residency-diagnostic-qualification.report.json.zst`, decoded
SHA-256:
`4d1ce2cb57e8dc16381d2db833f5acbf920c9ac4e9cb3da6318ece5360d3d9ba`.

### Shared-window reference audit and bounded-prefix correction

The complete encoder ownership audit found a reference-lifetime defect in
copied threaded input. `Common::win` constructed a shared slice over the
entire allocation while the hash producer could borrow its append tail
mutably. Actual byte accesses were disjoint, but the full shared reference
still overlapped the mutable tail. The slide locks alone do not prevent
append. This is a source-level safety finding, not an observed native
corruption or a newly measured performance improvement.

The correction constructs each prefix directly from its raw base and its
owner's published cursor/availability. Hash, BT, both mixer paths, both Skip
paths and the public MT window accessor are covered. Mixer availability is
restored by one because GetMatches decrements it before advancing the cursor.
History remains available for match comparisons and stored LZMA2 chunks.
The cached parser keeps its raw allocation descriptor and bounded per-byte
accesses. No buffer geometry, thread scheduling or upstream algorithm changes.

The audit also covers slides, direct input, stop/reuse, retained availability
after failure parking, and synthetic fallback mutation order. Saturating
physical bounds preserve oversized synthetic-state behavior; they do not
replace the producer's publication proof. Full-feature unit, encoder
differential, portable encoder unit and guard-page batches now pass on the
frozen Linux source with this encoder-only correction. Source hashes remained
unchanged throughout validation; later decoder edits are outside this batch.
Four regressions cover deterministic concurrent append, bounded endpoints,
stored chunks/reuse and parked physical bounds. Miri remains a final-capstone gate.
The final CI selectors now also include the previously omitted held-record
fallback and both skip-span differential tests; no Miri run has occurred.

Sevenz evidence (decoded SHA-256):

- `encoder-unsafe-lifetime-offline-audit.json.zst`:
  `2de6caa19167d6dad39f2b6eeae09b0b6063550d721c3d52b34311ea28c5c033`.
- `encoder-window-reference-endpoints-audit.json.zst`:
  `c181f6f01407bdd6f06e1d4a34602a96d2fa16c743da3e9aa5901f8ad7a01824`.
- `published-window-native-validation.report.json.zst`:
  `23d08c8f082e59fabceb199b6a499f51bfc8e363bda3bc924b6e1d4a03285d2f`.

### Windows borrowed delivery: complete comparisons and CPU accounting

All 294 operations completed correctly: seven cases, three arms, one warmup
and six balanced repetitions, with separate plain and traced operations.
All seven plain wall-time and peak-working-set medians pass the 3% limits
against both the preceding product and SDK. The candidate's wall changes
versus the preceding product range from -1.11% to +0.02%; this is a small
delivery improvement, not a large new throughput win. Full accounting of
all 147 traces now passes: complete paired owned-thread intervals, zero lost
events/buffers, and union ISR/DPC overlap subtracted from scheduled CPU.
Charged CPU remains separate and does not substitute for this measurement.

| Case | Wall vs SDK | Scheduled CPU vs SDK | Peak working set vs SDK |
|---|---:|---:|---:|
| Tiny direct | -0.13% | -0.47% | -14.35% |
| Tiny streaming | +0.25% | +0.09% | -8.23% |
| Ordinary | -0.54% | -0.69% | -7.93% |
| Parallel | +1.40% | -0.56% | -0.11% |
| Filter | -6.99% | -5.99% | -8.99% |
| COPY | -38.23% | -42.24% | -23.25% |
| AES | -39.41% | -41.88% | -39.31% |

All three metrics also pass against the preceding product; the largest
increase is +0.17% working set. This closes this fixed Windows cohort only,
not current-source fleet validation. The separate Linux RSS exceptions remain
scoped operator acceptances. All 294 operations, warmups, six balanced orders,
raw scheduled intervals and interrupt overlaps are retained. Earlier processing
failures and their recovery lineage remain recorded; no measurement was rerun
or discarded. All 147 original XML hashes match their lossless archive
round trips. Absolute runtime paths are sanitized in the repository artifact.

Sevenz `windows-nonsolid-t1-borrowed.accounting.json.zst`, decoded SHA-256:
`c8a00ac748ad740cc9cedab9dbcb6c7cbe147e61b546dd94e584d7309a0770de`.

Sevenz `windows-nonsolid-t1-borrowed.plain.json.zst`, decoded SHA-256:
`bdd80c7c97fca232fdebf036055f005682faa7e67591931b1189f937dad56374`.

### Complete nine-profile T8 source synthesis

All nine existing profiles and 117 thread summaries are retained. In each
run, the longest BT worker's sampled CPU is 97.3–98.1% of that same run's
process wall time, corroborated by paired scheduled intervals. This is
positive evidence of sustained worker execution, not an inference that
unobserved scheduling gaps are idle. It does not identify a final block or
prove memory latency, an instruction bottleneck, or a topology remedy.

Against the official SDK, the current build's broad parser family plus
consumer accounts for +1.851 mean sampled CPU seconds, out of +2.504 total;
the BT difference is +0.863 seconds with offsets elsewhere. Leaf-level
parser differences are not compared across different inlining boundaries.
Against the preceding product, total mean sampled CPU changes by -0.069
seconds, with redistribution among parser, consumer, BT and hash. These
profiled populations do not replace the unprofiled acceptance controls.

The complete source inventory covers admission, hash publication, BT
normalization and kernel entry, held records and parser, exact Vec return,
terminal output and teardown. Allocation ownership and long ordered-output
waits remain hypotheses pending the already-running qualified cohort; small
CPU samples do not bound wait time. No new timing cohort or speculative
queue, kernel or topology change follows from this synthesis alone.

Sevenz `t8-nine-profile-whole-path-synthesis.json.zst`, decoded SHA-256:
`584bcc0f35777113eb10046fa9c298f5a95ad0a3fed37ca4cb9088dfe050320a`.

### Complete allocation attribution and corrected profile replay

The fixed 120-operation cohort and all 36 allocation traces now pass complete
replay, with 922 sampled snapshots and 35 explicitly unavailable snapshots.
All warmups, repetitions, phases and modes remain in the evidence. The
corrected nine-profile analysis also completed; category totals are unchanged,
and no loss/throttle records or stderr warnings were detected. This does not
prove complete scheduling coverage or change acceptance results.

Output-buffer residency explains the observed modes substantially better than
finder geometry. In current T4 measured repetition 0, snapshots 8 and 12 show
sampled residency rising from 297.160 to 325.457 MiB while output-owned pages
rise from 56.062 to 84.352 MiB. Finder tables and input remain fixed. Current
L3 measured repetitions 0 and 1 likewise show 42.492 versus 28.203 MiB of
output-owned pages with unchanged finder/input geometry. Old product and SDK
also exhibit these modes; this is not a current-only allocation regression.

T8 current measured repetition 0 shows a later block finish at 3.737447 s
but wait until 4.755228 s for ordered writing, while the first block finishes
at 4.708479 s. Continued encoding acquires fresh output slots during that
backlog. All T8 traces retain continuous unions of observed encode scopes;
final encode-to-final-write tails are about 6.6–25.1 ms. These are inclusive
wall observations, not CPU utilization proofs. A slot cap would alter the
upstream pipeline and is not justified as a throughput-preserving correction.

Capacity is not residency: an SDK T4 low-mode run has a third 32.031 MiB
allocation that writes only the one-byte EOF. Its output sample remains
55.957 MiB. Nor can independent category maxima be summed: current T8 output
residency rises between snapshots 10 and 11 while total sampled residency
falls because input residency drops by 64 MiB. Snapshot queries can span
483 ms, and ambiguity remains explicit. Allocation identities do not establish
per-slot page counts, first touch or allocator retention after free.

The complete `ThreadFunc2` and output callback paths were re-read against the
pinned SDK: read-before-admission, LIFO free-list reuse, dynamic ordered writer
and release-after-write ordering are preserved. No missing whole-path port or
additional full-block copy was identified here. T8 wall +3.56% and L3 RSS
+7.88% in the separate acceptance controls remain open; the L3 timing/memory
trade is still for operator consideration. No benchmark was rerun in this
analysis round, and no performance closure is claimed for the safety fix.

Sevenz evidence (decoded SHA-256):

- `encoder-allocation-complete-cohort.json.zst`:
  `61c9b552948de5ed92261ba79e7c527fc83df6720bfe078872986db0d7aa73bd`.
- `t8-nine-profile-corrected-summary.json.zst`:
  `9c1322eaff7f7fe8baac07606c9e92e16bfad86b95409a2d4e7c441e9906bb2c`.

### T8 hardware diagnostic after the completed ownership audit

The remaining source/profile question is execution cost versus cache behavior
in the persistently running BT path. The preceding captures contain software
CPU-clock and scheduling counters only. A separate fixed nine-run hardware
diagnostic therefore reuses their exact whole-archive workload and frozen
old/current/official SDK binaries, with three rotated repetitions and no
affinity changes. It records hardware-cycle call chains and cycles,
instructions, cache references/misses and branch counters. This is diagnostic
evidence, not an instruction-count acceptance target or a new product change.

All nine runs pass archive/digest/integrity checks; 313 load checks detect no
build interference. The source correction validated above is not part of
these frozen binaries. All 114,881 sampled periods are now attributed from
flat instruction exports with exact sequence equality to the original
captures. Original call-chain rendering omitted many user leaves; direct
instruction export recovered them, and DSO-relative offsets resolve the SDK
against the established function ranges. No recording was repeated. Hybrid
PMU events remain
separate, including missing intervals and multiplex coverage; some numeric
intervals report near-zero running coverage. Scaled counts cannot be treated
as exact CPU time, and generic cache misses do not directly measure stalls.
The stat wrapper also includes recorder/measurement overhead; target wait4
wall, user/system CPU and RSS remain separate.

BT remains the largest individual category, but work shifts between PMUs.
Its core-PMU cycle fractions across the three repetitions are 71.16%, 58.96%
and 71.98% for current, versus 67.99%, 56.32% and 65.07% for SDK. Atom-PMU
fractions differ substantially. Parser-plus-consumer differences also change
direction between PMUs. These are not equal-work measurements and do not
identify a missing instruction sequence, cache remedy or topology policy.
No new product change or acceptance closure follows from this diagnostic.

Sevenz `t8-hardware-profile-cohort.report.json.zst`, decoded SHA-256:
`6f7323d7fe82d3ac07ed2d61298a4f962da05b42b98e275c8a38842968827777`.

Sevenz `t8-hardware-attribution.report.json.zst`, decoded SHA-256:
`7a03bd5cdc3128e83fc9dcd2a2d6912c072d2f85d58eb0546482016f1c2300ad`.

### T8 published-window controls and upstream consumer integration

The bounded published-window correction passed all 21 fixed acceptance
operations: one warmup and all six order permutations of preceding product,
candidate and official SDK. Candidate medians relative to SDK are wall
-1.0702%, CPU +0.6730% and RSS +2.4681%; archives are byte-identical to the
preceding product and every decoded digest matches. This cohort passes all
three limits, but the preceding product also runs close to SDK in this
cohort. The earlier +3.56% wall failure remains retained evidence; this run
alone does not establish a repeatable optimization gain.

The completed producer/consumer audit found no missing BT assembly batch,
window-copy elimination or ordered-writer transition. The pinned SDK also
contains an alternative fused `MatchFinderMt_GetMatches_Bt4`, combining
`MixMatches3` and record consumption. The certified BT4 consumer now adopts
that fusion with position-relative history addressing. Other finder widths,
indexed fallback, record certification, short-output behavior and the bounded
published window remain unchanged. This is the SDK's alternative inline
implementation, not its active default implementation.

All-feature unit, encoder differential and portable encoder native batches
pass. The new producer-record tail test checks complete output sentinels and
consumer state against the independent indexed implementation. Miri and the
format/lint capstone remain deferred. The separate fixed 21-operation T8
cohort passes: wall +0.4829%, CPU +0.1185% and RSS -4.0583% versus official
SDK. Relative to the preceding validated product, wall improves 1.6383%
and CPU improves 0.7323%. All archives match that product byte for byte;
all decoded digests and integrity checks pass. All 688 build-interference
checks are clear. These checks detect builds, not all possible shared-host
activity; full distributions remain in the evidence. RSS has multiple
observed modes, so its median improvement is not a deterministic allocation
reduction. T4/T16 nonregression controls completed all 42 operations on the
exact same frozen binary, without a rebuild. Every integrity/digest check
passes, all 1,597 build-interference checks are clear, and all 124 source
identities remain unchanged at completion.

| Width | Wall versus SDK | CPU versus SDK | RSS versus SDK | Result |
| --- | ---: | ---: | ---: | --- |
| T4 | +0.3699% | +0.8002% | +9.6072% | Timing passes; existing RSS gap remains |
| T8 | +0.4829% | +0.1185% | -4.0583% | All three pass |
| T16 | +0.4984% | +1.3964% | +0.1417% | All three pass |

Both neighboring widths improve all three medians against the preceding
product. T4 preceding/candidate RSS is 325.127/324.975 MiB versus SDK
296.490 MiB; the candidate does not introduce this gap. Both products and
SDK retain low/high residency modes in the complete distributions. The
earlier T4 parity result is retained, but does not override this cohort's
RSS miss. T8 is closed for this Linux x86 workload; T4 RSS remains open,
and these measurements do not establish parity on other architectures.

Release disassembly confirms the certified branch contains inline hash
mixing, position-relative history addresses and pair copying without
per-element bounds checks. The outlined mixer calls remain only on the
indexed fallback. Candidate SHA-256:
`314d5b2ec1d3360f07b1ef5cd4b7c174a6a0c68e80a4003b85fca757f3f3a001`.

Sevenz evidence (decoded SHA-256):

- `t8-published-window.report.json.zst`:
  `866f2d0362b9107d2a18b5a8bb53bb6618726ca7bf219a5c44971ca0353c0c65`.
- `t8-published-window.raw.json.zst`:
  `22da568893249a9cceb0700fc65d9077291b395d0c412acd93158c08bf8e5605`.
- `t8-fused-bt4-native.report.json.zst`:
  `4babdc42a1c9300a69245fa04b0f6d9d8458e0a1a86485bd293a7d2268e475d6`.
- `t8-fused-bt4-codegen.json.zst`:
  `9a3ff5519e177c47112d94bc6a5c11abfab63ccbf5bf8cb16c7ec55478706d86`.
- `t8-fused-bt4.report.json.zst`:
  `83aff2ee0e8210998ab2a45ac91b8b02d5ec56343044c59d6fe09b1afcb87430`.
- `t8-fused-bt4.raw.json.zst`:
  `1d1aea0ebc6c8db68ee17ba809d63ef2e242c12bdc35863ccde737839d7fff9d`.
- `t8-fused-bt4-neighbors.report.json.zst`:
  `d047ee0b78b94e62fd754f0f73ea1da7d7b23d2d8f038c79e87e68475f0b0158`.
- `t8-fused-bt4-neighbors.raw.json.zst`:
  `c61cca06d790c511c09d3f2cbf49e3ff08721b0dd12ec23a6f53eed8c83ac075`.

### Completed-output retention and caller-output XZ filtering

The full T4 allocation trace narrows the output high-water mode to lifetime,
not extra admission. In measured diagnostic repetition 0, slot 1 finishes
at 5.652967 s, slot 2 starts at 5.659919 s, and ordered writes release slots
0 and 1 at 5.673482/5.683472 s. Slot 0 is then idle for the rest of the
stream, but its delivered allocation stays resident while slot 2 fills.
The corresponding output sample grows from about 56 to 84 MiB with finder
and input storage unchanged. Snapshot query intervals and ambiguity remain
in the complete attribution evidence; these are not per-instant page proofs.

Re-reading complete `ThreadFunc2` and `Lzma2Enc_MtCallback_Code/Write`
confirms the block admission, LIFO reuse and ordered delivery port. The new
policy intentionally differs from SDK permanent per-slot retention: after
delivery, release that slot's allocation only when the total allocated
output slots exceeds the number of block coders. Ordinary coder reuse is
kept. Pending/encoding slots, admission limits, block size and byte order
are unchanged. A per-run relaxed atomic count is a retention heuristic,
not a safety or admission bound. Ownership-taking sinks and failures update
the count; tests cover retained and transferred buffers, allocation failure,
write failure and a sink taking ownership before returning an error.

The independent XZ audit ports liblzma caller-output filtering into the
product block decoder. It retains the unfiltered dictionary and filter
ordering, checksums and size caps, replacing the 64 KiB private intermediate
with bounded tiny-output storage. Caller buffers and the benchmark probe
are unchanged. This removes a real product copy/storage layer, but cannot
alone be claimed to close the previously observed whole-process RSS gap.

All-feature units (234), the combined encoder/XZ differential batch and
portable encoder units pass. The first staging preflight failed before any
source mutation because a destination was a directory; its log is retained.
Corrected preflight checks both baseline and candidate file hashes before
overlay. Baseline/candidate XZ binaries and the encoder binary are frozen;
fixed T4 and ten-filter measurements run sequentially, followed by a broader
encoder matrix only if T4 passes.

The complete fixed T4 cohort (warmup plus six balanced permutations; 21
operations) passes against SDK: wall +0.5445%, CPU +0.7012%, RSS +0.2445%.
Median RSS falls from 324.646 to 297.092 MiB, versus SDK 296.367 MiB.
Against the preceding product, wall is +0.5636%, CPU +0.2626%, and RSS
-8.4876%. Plain archive bytes are identical between product builds; decoded
hashes and integrity checks pass. The full distributions remain in evidence,
including a slow baseline observation; no observations were discarded.
This closes this Linux x86 L5 T4 RSS cell. The complete broader encoder
regression cohort also passes; it does not establish other-host parity.

All 126 operations in the six-case follow-up complete with integrity and
decoded-hash checks. All 5,896 build-interference checks are clear and the
full 124-file source inventory is unchanged afterward. Unencrypted product
archives remain byte-identical. Ratios below are median percentages versus SDK:

| Case | Wall | CPU | RSS |
| --- | ---: | ---: | ---: |
| L5 T8 | +1.2993% | -0.3177% | -7.6795% |
| L5 T16 | +0.3748% | +0.4429% | +0.2244% |
| AES L5 T16 | -0.0155% | +1.0059% | +0.0709% |
| Zeros L5 T4 | -21.7883% | -12.5227% | -0.1046% |
| L1 T2 | -15.6394% | -15.8191% | -4.2033% |
| L3 T2 | -24.1163% | -24.0288% | -9.0016% |

Every metric also stays within 3% of the preceding product; the largest
increase is L1 T2 wall +1.2746%. The historical accepted T2 RSS exception
remains recorded, although this current L3 T2 cohort improves RSS. All
observations remain, including slow runs and distinct residency modes.
The raw harness's descriptive note inherits an older five-case label;
the explicit case list, orders and 126 samples establish this cohort's scope.

All ten XZ filter cases complete with timing within 3% of the pinned xz
reference, but whole-process RSS remains approximately 26–27% above xz.
The caller-output port is retained as a partial improvement. Complete
allocation and resident-mapping attribution is the next diagnostic; the
remaining gap is not assigned to the decoder heap without that evidence.

Sevenz evidence (decoded SHA-256):

- `rss-batch-native.report.json.zst`:
  `918eaaa52b91d09a7e2a18458d944e88147405eff021eed4c15c04eebac11062`.
- `rss-batch-build.manifest.json.zst`:
  `56c42584039af48cc6666926735f056780d8a8efc3c0bc566ae172a016ebf96d`.
- `t4-output-retention.report.json.zst`:
  `d451ec833ff6249c8c40221ab4d5769beaddac010c9f8d8ec315658aaf782c9d`.
- `t4-output-retention.raw.json.zst`:
  `8d33355b434aed28fc80ef4ac4e0eb72f430e98dc26cfe036cb9609270e936b1`.
- `xz-caller-output.report.json.zst`:
  `82c5718e19a75404bbbaecab14599a0fe3eba7f0574c8cf58d655ec45b26c0d1`.
- `xz-caller-output.raw.json.zst`:
  `9c7771040e2d551b7cddf8f4c9099cde3f5593d4ce5508b0134d74988831008d`.
- `output-retention-widths.report.json.zst`:
  `f34b8bfa27f2f3e77338c06299ed1f4bc193e8ffa900c2a2da907868b449994d`.
- `output-retention-widths.raw.json.zst`:
  `00c9ba4d6391ffaf7bb9bb084dcafd2aa565db663fd7d5b5cbea1a275a04c70e`.
- `output-retention-widths.source-after.json.zst`:
  `ae17f8d0ad40b97d78f3b87c5c91361f1966225ddacada011256eb4240dd3086`.

### Decoder RSS attribution and borrowed-delivery confirmation

The XZ allocation diagnostic completes all 30 fixed operations and 90
qualified snapshots. Every filter has the same active owner-page accounting:
dictionary 8,196 KiB, Source 64 KiB, probabilities 28 KiB, shared boundary
4 KiB, caller input 1,028 KiB, caller output 68 KiB, and anonymous residual
68 KiB. These are intersecting resident pages, not exact exclusive allocation
sizes. All 50 tagged generations are freed; no tiny pending buffer is used.
The post-shot residual retains 96 KiB of pages formerly intersecting Source
and probabilities, not a live decoder allocation or a multi-MiB leak.
The instrumented image cannot establish the original executable's mapped
RSS. A separate fixed 20-operation control captures the unmodified product
and xz at a qualified compressed-input position; its interpretation is
pending. Consumer buffers and benchmark behavior remain unchanged.

The Linux borrowed-delivery build passes 230 LZMA and 248 sevenz library
tests plus the SDK and positional integration targets. Its complete 42-operation
cohort preserves correctness but does not close non-solid T1 RSS:

| Case | Wall vs SDK | CPU vs SDK | RSS vs SDK | RSS vs prior |
| --- | ---: | ---: | ---: | ---: |
| Tiny direct T1 | -0.7983% | -0.8104% | +13.1370% | +2.9837% |
| Tiny streaming T1 | -1.4290% | -1.4329% | +15.5752% | +3.3461% |

Timing improves about 0.7% against the prior product, but streaming RSS
exceeds the product-regression limit. The faithful borrowed path is retained
for the ongoing audit, not accepted as a completed RSS fix. Current candidate:
`f661fad361e71358e27abf401b6b8b89de45fe54ff3ff5ed1d1ff621ed065078`.
Whole-process attribution must distinguish archive/decoder storage, the
streaming consumer's retained CRC metadata, and resident executable/library
pages. Resident executable pages do not establish executed instructions.

Full evidence is preserved as compressed JSON bundles, including raw events,
mapping snapshots, validation logs and lineage. Home paths become role labels;
original hashes remain recorded, and sanitized text is not represented as
byte-identical to its private original. Decoded bundle SHA-256 values:

- `xz-residency-attribution-v2.evidence.json.zst`:
  `61bf0cc01182ece26584e9c4abf1a9fdd495dbe0adba6a33843d166cd140cfe9`.
- `xz-original-mapping-control-v3b.evidence.json.zst`:
  `fc6a425a96fedc2371d5d0f92e6444d275006180ef82582a030155147cb0c2b5`.
- `nonsolid-borrowed-linux-v2.evidence.json.zst`:
  `649c44545b947941a23d8306ac2d48837376c8a981e7fffd437daabfd02d6ec8`.
- `xz-residency-v2-interpretation.json.zst`:
  `c2df6df6bd1880d79a5086a43f70de507765e17d55751ba0868641bdbd745b93`.
- `x86-nonsolid-rss-attribution-handoff.json.zst`:
  `9910f1944b61c0327010574d1957bb6cbf2915072348bc7b9fc066671804d4d5`.

### Whole-path metadata and executable residency audit

The original XZ executable control completes all 20 operations. Its excess
is 2,784–3,196 KiB: 1,628–2,032 KiB in file mappings and 1,152–1,172 KiB in
anonymous mappings. The separate allocation diagnostic attributes about
1,096 KiB to caller input/output pages. These checkpoints use different
progress measures; their agreement is evidence of ownership, not an exact
cross-process subtraction. The original executable is stripped. A companion
symbol build must prove loaded-segment identity before supplying names for
its resident pages. Resident pages alone do not prove executed code.

The non-solid original-image snapshots keep heap and anonymous allocations
unchanged between the preceding product and borrowed-delivery candidate.
The candidate adds 120 KiB of executable pages in direct delivery and
260–264 KiB in streaming delivery, with slightly lower stack residency.
Against SDK, the direct candidate has 720 KiB more heap and 700 KiB more
other anonymous pages. The public entries allocation is a candidate owner
of the 772 KiB mapping, pending pointer-qualified allocation attribution.
Streaming additionally retains the consumer's expected/completed CRC
tables; these are not decoder storage and the consumer is not being changed
to improve a library result.

The complete metadata representation audit covers `Archive`, `ArchiveEntry`,
`StreamMap`, `Block`, `Coder`, `SmallSlice`, names, header lifetime, graph
validation, sub-stream readers, positional consumers and checksum fallback.
The upstream comparison covers `CSzAr`/`CSzArEx` in public-domain `C/7z.h`
and observes `CFolders`/`CDatabase`, `ReadUnpackInfo`, `ReadSubStreamsInfo`
and `FillLinks` in the C++ frontend; LGPL implementation text is not copied.
SDK retains packed coder records and optional metadata columns. Our public
entry fields, coder vectors and mapping slice APIs constrain a direct
representation port. Lazy expansion would not remove storage when a
consumer requests `archive()` before decoding, as this cohort does.

Prior work already removes simple-folder graph allocations, stores small
properties inline, builds name lookup lazily and drops decoded headers
before final maps. Sub-stream sizes and checksums have live consumers,
including generic block CRC fallback and positional extraction; deleting
them or deriving them from mutable public entry fields is not a proven
behavior-preserving fix. The next allocation census qualifies all retained
metadata owners before choosing the complete compatible change set.

- `x86-nonsolid-original-mapping-attribution.json.zst`, decoded SHA-256:
  `b257b39cbf5374a7740ac9ab266d328d41db7541fefd7d9cc5c541fcc333ac2a`.

The metadata census now completes both delivery modes with four qualified
snapshots and unchanged original source. There are 16,403 retained library
allocations requesting 2,537,666 bytes; their resident page union is
2,830,336 bytes in both modes at both snapshots. Entries consume 787,200
bytes (8,200 records), blocks 524,288 bytes, coder vectors 393,216 bytes,
names 241,986 bytes and maps 393,344 bytes. Pack sizes and sub-stream sizes
and CRCs each consume 65,536 bytes; the checksum bitset consumes 1,024.
Names and vectors have no spare requested capacity. This fixture has no
separate graph/property allocations. Allocator slack and shared pages are
not counted as additional requested bytes. Streaming consumer CRC tables
remain separately attributed, including a 131,072-byte completion vector.

The selected compatible batch is private coder-count compaction, wire-width
CRC storage and an identity representation for original one-entry-per-folder
sub-stream offsets. It preserves public types, full-width exceptional counts
and independent parsed snapshots. Its estimated approximately 224 KiB saving
is provisional and does not promise complete SDK RSS parity.

The XZ symbol companion fails loaded-byte identity, including executable
code, constants and relocations, rather than only build-ID metadata. No
symbols are transferred, and it is not a replacement acceptance binary.
The direct-output port remains; the process-RSS gate remains unmet.

Additional complete evidence bundles, decoded SHA-256:

- `nonsolid-original-mapping-v1.evidence.json.zst`:
  `c0de58edd74885e71d4909cb7ef56fdc2e04f128727b3bd7e6596249de2ce216`.
- `nonsolid-metadata-census-v1.evidence.json.zst`:
  `bf6f69700d17ef13f49cd7d890129336953a292c7baed37a837f5d25928052d7`.
- `xz-symbol-companion-v1.evidence.json.zst`:
  `1c87466a0d2054b4a11f2773660f126b7bee373f469dacff1caa1501c5df0fb3`.

### Signed checkpoint preparation, 2026-10-14

The LZMA formatting and all-feature workspace Clippy gates pass. The full
default workspace sweep passes 435 tests; the two existing guard-page tests
remain reserved for their separate CI job. Portable, no-std, native crypto,
dual crypto, encoder-only and encoder/XZ suites pass, as does the encoder/XZ
compile-only configuration. Nextest is unavailable on the validation host,
so these sweeps use Cargo with `--no-fail-fast`. The initial workspace
attempt lacked a complete SDK assembly source tree; the successful rerun
uses the exact pinned upstream commit. Both attempts are retained in
`checkpoint-native-validation.json.zst`. Local Miri remains deferred until
the performance arc ends; this checkpoint does not claim Miri validation.

The non-solid metadata batch has not yet received a new acceptance cohort.
Its initial native run found a new test asserting an internal checksum error
after the public API had already attached the block location. The test now
asserts the public checksum classification and exact location. Independent
review also found and corrected public CRC narrowing in the new positional
API, with high-bit mutation coverage. These changes do not close the RSS gap.

Before publication, compressed evidence is inspected as decoded content.
Absolute reference-tool and temporary paths are replaced with role labels;
measurements and original hashes are retained. The six baseline/current
evidence hash mappings are recorded in `checkpoint-privacy-mappings.json.zst`.
The lifecycle validation evidence now decodes to
`6fc63c6c1905c3645d42ac9a4e1e3524baf262e3402927854f55a6f9aef19bf8`;
its original decoded hash is retained inside the evidence. Fresh checkpoint
branches exclude earlier local commits containing unsanitized evidence and
preserve the newer upstream benchmark documentation.


### Non-solid T1 checkpoint follow-through, 2026-10-14

The Git-pinned checkpoint cohort completed all 42 timed operations and six
correctness checks with every sample retained. Direct delivery is -1.34%
wall, -1.36% CPU and +6.25% RSS versus SDK; streaming is -0.43% wall,
-0.43% CPU and +11.94% RSS. RSS is 1.25%/1.15% lower than the fixed
preceding product. These are improvements, not closure of the 3% RSS gate.

Exact original-image snapshots attribute the remaining excess principally
to retained metadata. Direct product mappings hold 1,980 KiB heap plus
824 KiB other anonymous memory; SDK holds 1,484 plus 124 KiB. Product
file mappings are 5,996 versus SDK 5,872 KiB in that diagnostic. Streaming
retains caller CRC tables and later creates a separate caller verification
set. The unchanged benchmark retains all of those checks. Mapping diagnostics
are not timing acceptance or proof that every resident code page executed.

The next compatible representation follows the SDK compact-coder principle:
a safe 24-byte inline record retains every 15-byte method ID, up to seven
property bytes and common stream counts. Boxed exceptional records preserve
full-width counts and longer properties. Native tests verify the layout,
all ID lengths, property boundaries, clone independence and logical equality
across representation transitions. No unchecked union or pointer tagging is
introduced. The common one-coder allocation saves approximately 128 KiB across
this fixture; exceptional/AES metadata may instead grow, so this is not a
claim of uniform memory reduction across archive types.

The all-feature native sweep passes 273 library tests and every existing
integration target. Its new large-offset target exposed a genuine existing
compatibility gap: SDK accepts a wholly omitted optional SubStreamsInfo
section, while this reader rejected it. Explicit-section large-offset tests
pass through both ordinary and positional readers; the original valid case
is retained separately, and a faithful defaulting fix is pending validation.
The inherited-CRC correction is validated by the full suite. Failed compiler
integration attempts and the original valid-archive failure are retained.

Sevenz evidence bundles, decoded SHA-256:

- `nonsolid-metadata-checkpoint-v2.evidence.json.zst`:
  `d0a1abc4e00fe8ba3ff15f087c68e8ab3e3e2880d110854b0443e345b8498534`.
- `nonsolid-metadata-mapping-v2.evidence.json.zst`:
  `5161a91b5eca63f208732b447c3cae40a76724f5b2380feb31fceaea6c691f72`.
- `nonsolid-packed-coder-validation.json.zst`:
  `428077e4e52aa0818cd8a914a6e29bdffd5a741a20d386db668daf773770517e`.


The compact-coder cohort subsequently completed all 42 operations with six
correctness checks and no interference-guard failures. The operator accepted
the measured residuals and chose to preserve the public API:

| Non-solid T1 mode | Product RSS | SDK RSS | Extra RSS | Wall delta | CPU delta |
| --- | ---: | ---: | ---: | ---: | ---: |
| Direct | 8.125 MiB | 7.705 MiB | 0.420 MiB / 5.45% | -0.192% | -0.193% |
| Streaming | 8.637 MiB | 7.773 MiB | 0.863 MiB / 11.11% | +0.174% | +0.162% |

Versus the fixed preceding product, direct RSS improves 3.14%; streaming
RSS is +0.29%. Timing differences versus that product stay within 0.04%.
The decision closes this measured lane as an exception, not as 3% RSS parity.
It does not accept leaks, accumulating retention or future regressions.
All raw samples, correctness checks and benchmark drivers are retained in
sevenz `nonsolid-packed-coder-v3.evidence.json.zst`, decoded SHA-256
`4a63e143c3aa269a84aca440d3fea79e740b93b270b70069aff2c139574320d3`.

The separate wholly omitted SubStreamsInfo port then passes the library,
container and security suites, both default large-offset tests and all four
new omitted-section regressions, including independent snapshots after public
mutation and exact explicit-malformed-section errors. That later correction
is not part of the frozen performance binary above.

The subsequent XZ writer audit covered startup, steady-state delivery, filter
tails, EOF, destination errors and both directions of the encoder pipe.
Following `Xz_CompressBlock`'s streamed delivery removes the accumulated output
vector from the writer path. Bounded output channels share wakeups for input
room, output availability and worker exit, so full queues cannot wait solely
on each other. Buffered encoder APIs retain their output contract.

Twenty-four interleaved operations (one warmup and three measured samples per
arm and preset) completed on the Mac during normal use. All output sizes
match the previous product build. Medians are:

| Preset | Previous RSS | Candidate RSS | xz RSS | Candidate extra RSS | Tracked allocation saved |
| --- | ---: | ---: | ---: | ---: | ---: |
| P9 | 865.59 MiB | 744.30 MiB | 674.75 MiB | 69.55 MiB | 62.56 MiB |
| P5 | 122.80 MiB | 108.05 MiB | 94.77 MiB | 13.28 MiB | 7.04 MiB |

Both memory differences fit the previously accepted bounded allowances
(P9 99.66 MiB; P5 23.75 MiB). Candidate wall/CPU are 58.22%/58.76% below xz
at P9 and 62.74%/62.67% below xz at P5. Against the previous product build,
however, medians are 9.53%/7.93% higher at P9 and 3.84%/3.83% higher at P5.
Compiler load differed substantially between arms; every sample is retained.
These results establish the memory reduction but do not establish absence
of a baseline-relative timing regression. That investigation remains open.

All four pipe tests and all 26 XZ/LZMA2 integration tests pass, including
full-duplex progress, worker-panic wakeup, cancellation during blocked output,
partial destination writes, original I/O errors and external decoder checks.
The three new lifecycle tests were added after the frozen benchmark build;
the evidence records both source identities. No new unsafe code or dependency
is introduced by this writer change. The separate allocator pressure-relief
prototype remains unaccepted. Tree/L9 timing and T4/T8 RSS remain open.

Sanitized samples, background process roles, binary/source hashes and analysis
are in `mac-fleet-xz-pipe-controls.json.zst`, decoded SHA-256
`6b7695818d7f8bd59a2bb62784f27e5d757e5058f953c78a830ddd9e3b74e2ec`.


A follow-up P9 comparison used four measured samples per product arm in
ABBA order, without the much longer xz run separating the arms. All eight
operations succeeded with identical output sizes. Candidate medians were
44.968 s wall, 42.757 s CPU and 744.24 MiB RSS; the previous product measured
46.702 s, 43.400 s and 865.52 MiB. This is -3.71% wall, -1.48% CPU and
-14.01% RSS. The original noisy cohort remains above, including its failures
to meet the baseline timing threshold. The follow-up supports the P9 change
on this in-use Mac; P5 baseline timing still needs its own follow-up.

A separately symbolized diagnostic build identifies the unchanged binary-tree
match walk as the main sampled CPU leaf. Pipe functions did not reach five
leaf samples. Waiting caller samples are not counted as CPU work. This is a
hotspot diagnosis, not an acceptance timing result.

Balanced P9 samples and observed background load are retained in
`mac-fleet-xz-pipe-balanced.json.zst`, decoded SHA-256
`e956a5f3ef2633ec76f39cc7aa6d988051c326dd184468accdb9c4396686188e`.


The balanced P5 follow-up also passes: candidate medians are 19.474 s wall,
19.468 s CPU and 108.05 MiB RSS, versus 19.975 s, 19.968 s and 122.90 MiB
for the previous product. All eight operations succeeded with the same output
size. Wall/CPU improve 2.51%/2.50%; RSS falls 12.08%. Evidence is
`mac-fleet-xz-p5-balanced.json.zst`, decoded SHA-256
`d91c8b2a53a40408f44414786a45902af5f67f870366dff1bfa8e67e76fec1b6`.

Ranking the full resumed matrix by excess bytes identified larger unaccepted
XZ parallel-encoder overages than the remaining sevenz T4/T8 rows. The audit
found both an aggregate packed-batch copy and a fresh allocation/shrink cycle
for every block. `XzEnc_MtCallback_Write` delivers blocks directly; its Code
counterpart retains a fixed destination. The writer now follows both lifetime
rules. Buffered APIs preserve their result contract. A transition to one
streaming block releases the now-unusable batch coders and packed buffers.

One warmup and three measured repetitions per arm, rotating the previous
product, candidate and xz, give the following P5 medians:

| Width | Previous RSS | Candidate RSS | xz RSS | RSS saved | Wall vs previous | CPU vs previous |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| T2 | 467.34 MiB | 257.28 MiB | 319.86 MiB | 210.06 MiB | -8.51% | -8.27% |
| T4 | 716.67 MiB | 509.11 MiB | 616.95 MiB | 207.56 MiB | -0.34% | -0.35% |

Both rows beat xz on wall, CPU and RSS, with identical product output sizes.
All 233 library tests and 27 XZ/LZMA2 integration tests pass, including
parallel short-write retry, block-size transitions, every filter/check chain,
external decoding and exact buffered-versus-writer output. No new unsafe
code or dependency is needed for the XZ changes. The separate macOS surplus
page-release experiment is not accepted by this comparison.

The earlier direct-delivery-only cohort is retained: it saved 42/84 MiB but
its T4 wall/CPU medians were 3.70%/4.04% above the preceding product. The
combined fixed-buffer reuse result above supersedes that candidate, not its
evidence. Remaining presets, widths and filters are undergoing regression
controls; these two rows do not establish all-architecture parity.

All three MT cohorts and source/binary identities are in
`mac-fleet-xz-mt-output-controls.json.zst`, decoded SHA-256
`45b64c8e22e2ebebb912e6595062abf5e0814c7e269a5ad933f999cb7aeabae8`.

The remaining encoder matrix completed 23 scenarios and 276 operations
(one warmup plus three measured repetitions per arm). Every completed product
output size matches. All additional MT rows beat xz on wall, CPU and RSS;
their baseline timing deltas remain below 3%. Three baseline timing crossings
remain unresolved: P3 +3.55% wall / +3.71% CPU, P9 +3.15% / +3.16%, and
ARM64 filter encoding +3.35% / +3.21%. Earlier balanced P9 controls passed,
but that does not erase this newer crossing. P3 samples show substantial
variation and recorded background activity; adjacent balanced controls and
paired profiles are queued before attributing the crossing to a code path.
The complete matrix, including every sample and anonymized process-load
values, is `mac-fleet-xz-encoder-regression-controls.json.zst`, decoded SHA-256
`fafb49686ae46702484489e4d53842ada5b9e261b129e79d53608e334ec2508e`.

The decoder ownership audit found a distinct memory-bound defect in both
indexed and adaptive XZ scheduling: completed out-of-order blocks stopped
occupying admission slots while retaining their buffers. Indexed scheduling
now counts ready blocks with in-flight work. Adaptive scheduling also counts
ready blocks and partially delivered output against slots and input
backpressure. The latter retains the whole output block's cost until its last
byte is delivered. This prevents backlog growth behind a slow earlier block;
it does not claim a reduction in the one-worker packed-input allocation.
All 236 library and 56 selected integration tests passed after both fixes,
including three deterministic admission regressions and external XZ fixtures.
All 18 indexed-reader performance scenarios then passed the baseline 3%
wall/CPU/RSS limits in 216 operations, with matching decoded lengths and
CRC-32 values. This validates the scheduling change against the previous
product, not the unresolved T1 RSS gap against xz. The adaptive path has
correctness coverage but no dedicated performance lane in this harness.
Samples, source identities and validation counts are retained in
`mac-fleet-xz-admission-controls.json.zst`, decoded SHA-256
`9965ff8b11be0b669ce56bbd2660f6223592e5b9b7400e2127a8ab274bed5952`.

P3 follow-up preserved the initial four-repetition balanced failure (+9.06%
wall / +9.04% CPU) and collected paired native profiles. Both profiles are
dominated by `MatchFinder::hc_get_matches_spec`; the compared hot instruction
sequence is unchanged apart from relocated addresses, and the summaries show
no substantial new pipe leaf hotspot. The profiled candidate finished slightly
faster, but profiled runs are not acceptance measurements. An expanded,
adjacent ABBA comparison of the same frozen binaries used one warmup and eight
measured repetitions per arm. Its medians pass: wall 24.170917 versus 23.843244
seconds (+1.37%), CPU 24.178283 versus 23.838290 seconds (+1.43%), and RSS
43,335,680 versus 49,184,768 bytes (-11.89%). No further encoder source change
was made between these cohorts. Earlier crossings and observed timing
variability remain recorded rather than erased. P9 and ARM64-filter baseline
crossings are still undergoing their own adjacent comparisons.

Both P3 cohorts, the profile audit and profile hashes are retained in
`mac-fleet-xz-p3-controls.json.zst`, decoded SHA-256
`5d0ea0afa11a94d0849f74a173087893f2143587cc927e7f197eb33f7749cc1e`.

The P9 and ARM64-filter follow-up completed with one warmup and six measured
repetitions per arm, in adjacent alternating order. P9 passed against the
previous product: wall +2.26%, CPU +2.24%, RSS -14.03% (127,328,256 bytes
saved). ARM64 passed at wall -1.31%, CPU +0.13%, RSS -8.24%. Output lengths
matched in every operation. These results concern the frozen encoder candidate;
later decoder changes have a separate cohort. Background compiler activity
and earlier failed cohorts remain recorded. All 28 operations and identities
are retained in `mac-fleet-xz-remaining-controls.json.zst`.

The indexed one-worker reader now uses the bounded-input structure of
`XzDecMt_Decode_ST` in pinned `C/XzDec.c`: refill at most 256 KiB of packed
input while retaining decoder state. It preserves whole-block verified output,
shares header/size/end-marker/filter/check validation with the buffered worker,
and validates padding before decoding. Memory-limit admission remains the
existing conservative bound. All 293 selected native tests passed, including
short and interrupted source reads across multiple input refills, bounded
allocation assertions, and malformed-block error/delivery comparison against
the buffered worker. The subsequent 18-row cohort retained all 216 operations
with matching product lengths and CRC-32 values. T1 saved 43,859,968 bytes on
the 1 GiB fixture, 29,196,288 bytes on the 16 MiB-block fixture, and 43,843,584
bytes on the other 256 MiB fixture. Wall and CPU were no worse than baseline
on all three. Compared with xz, remaining RSS excess is 10,108,928 to
18,513,920 bytes while wall and CPU are roughly 39% lower. This remaining
whole-block verified-output cost is recorded for operator consideration; it
is not a newly accepted exception.

Six multi-worker rows crossed at least one baseline threshold during this
cohort, which includes recorded compiler/linker interference. They remain
failed measurements. Samples are retained in
`mac-fleet-xz-bounded-input-controls.json.zst`, decoded SHA-256
`f2435339dc1794ddb182e39ccaa9ae51ecb4816c420bb94ddf532d876c9be131`.

The allocation audit found geometric packed-input growth in both indexed
and adaptive dispatch. The indexed fixture starts with a 14,666,760-byte
packed block and later blocks are slightly larger; reusing that buffer could
double its capacity, matching the approximately one-packed-block steps in
measured peak allocation. Both paths now reserve the known size exactly.
Deterministic tests seed a buffer one byte short of the required extent and
assert that capacity does not double. All 294 selected native tests pass.
The new 18-row cohort uses six repetitions for the prior crossings and three
elsewhere. All 270 operations preserve product output length and CRC32;
all 18 rows pass the 3% wall/CPU/RSS limits against the frozen previous
product. The 1 GiB T8 row saves 126 MiB with wall and CPU within 0.01%.
The T1 rows still retain 10–18 MiB more than xz for verified block delivery;
that proposed exception is not accepted. Their wall and CPU are 37–38%
lower than xz. Sanitized samples and identities are preserved in
`mac-fleet-xz-exact-packed-controls.json.zst`, decoded SHA-256
`2d67af9ff83fad852fd964c429eebbc82e8d2e40fc2bf6ea4da28cb5fba9be00`.

### Ordered encoder output: focused Mac gates

The broad ARM sequence was stopped after four completed rows to prioritize
individual failures. Its remaining finished archive passed integrity checks;
the partial sequence and earlier failed candidates remain preserved locally.
The ARM kernel remains diagnostic: its solid-tree improvement does not justify
its T2 regression against the shipping portable walk.

Memory profiles showed resident empty allocator regions after surplus-output
release. A first-block-only direct-output candidate failed T8 RSS and is
retained in the evidence. The next candidate permits any block that already
owns the next ordered write turn to use `Lzma2Enc_EncodeMt1`'s streaming-output
mode. Other workers continue encoding to buffers. The write turn remains
reserved through encoding and ordered draining; admission and output order
are unchanged. This is a deliberate extension of the SDK dispatcher, using
the existing SDK subblock encoder, not a claim of an identical dispatcher port.
Each Rust coder owns its chunk scratch. There is no shared SDK temporary-buffer
race and no new unsafe block or dependency.

Byte comparisons cover borrowed and owned input, recycled output indices,
and write errors. The four existing dispatcher tests also pass. The candidate
is still behind `kernel-ab` and `LZMA_TURBO_ORDERED_DIRECT=1`; default delivery
is unchanged pending checkpoint validation.

Six measured repeats per arm pass both focused median gates. T8 saves
37,298,176 bytes against the frozen product; versus 7zz, wall is 0.111% lower,
CPU 0.478% higher, and RSS 0.266% lower. T4 saves 11,968,512 bytes against the
frozen product; versus 7zz, wall is 3.023% lower, CPU 0.361% lower, and RSS
0.159% higher (491,520 bytes). T4 CPU is 1.192% higher than the frozen product,
within the 3% limit. All product archive hashes match. Compiler interference
and variable RSS samples are retained, not filtered; these are cohort median
results rather than promises about every individual run.

Evidence is in `mac-fleet-ordered-output-t8-controls.json.zst` (including the
failed first-block attempt and initial noisy cohort) and
`mac-fleet-ordered-output-t4-controls.json.zst`. Zeros T2/T4/T8/full-thread
controls also pass every baseline and reference wall/CPU/RSS threshold.
The full-thread checkpoint passes all baseline regression limits, but absolute
7zz timing gaps remain: solid-tree +3.39% wall / +4.66% CPU, L9 +4.26% / +4.63%,
and AES +5.32% / +5.25%. AES is +2.17% wall / +2.35% CPU against the frozen
product. Those are unresolved thresholds, not accepted exceptions. The complete
checkpoint is retained in `mac-fleet-ordered-output-checkpoint-controls.json.zst`;
zeros evidence is in `mac-fleet-ordered-output-zeros-controls.json.zst`.

Full-lifetime AES profiles include ciphertext delivery and pass integrity.
The BT walk and parser dominate both product profiles. AWS-LC CBC has 79
baseline and 72 candidate top-of-stack samples; memory copying has 47 and 46.
These perturbed samples are diagnostic counts, not elapsed-time comparisons.
Source inspection confirms the AES encoder still accumulates 64 KiB batches
independently of upstream write boundaries. There is no measured basis here
for changing the cipher backend or batching policy. Preserve the output-memory
gain while addressing the remaining encoder hotspot. At that checkpoint the
candidate remained diagnostic; the follow-up below records its promotion.

### ARM negative-offset traversal follow-up

The complete pinned SDK C walk, hand-tuned x86 assembly walk, Rust wrapper,
held-record consumer, `ReadMatchDistances` and `GetOptimum` were audited before
the next kernel edit. Parser decisions already follow the SDK. The dominant
BT path now has an ARM64 port of the hand-tuned negative-offset traversal:
child-first mismatch handling, direct/cyclic addressing and repeated-node
copying, with the existing bounded eight-byte match extension. There is no
thread-count or core-topology dispatch. Earlier C-shaped ports and failed
candidates remain available and their measurements are retained.

Six native differential tests pass against the checked oracle. Focused
three-repeat median controls, each after a warmup and with rotated arm order,
give these differences against 7zz (negative means less resource/time):

| Gate | Wall | CPU | RSS |
| --- | ---: | ---: | ---: |
| L5 T2 | -17.62% | -10.63% | +0.41% |
| Solid tree L5 full threads | -8.54% | -4.36% | -1.63% |
| L9 full threads | -16.34% | -11.14% | +0.03% |
| AES L5 full threads | -8.21% | -3.14% | -2.51% |
| L5 T4 | -9.46% | -4.24% | +0.12% |
| L5 T8 | -9.60% | -3.50% | +0.07% |

T4/T8 excess RSS is 376,832/409,600 bytes. All gates also pass against the
same-binary portable walk with ordered output enabled. Zeros at T2/T4/T8/full
threads pass every timing and RSS threshold; plaintext product archive hashes
match, and encrypted outputs pass integrity with unchanged sizes. Tree and
zeros had no detected compiler interference. T2, L9, AES and T4/T8 include
recorded background builds; their full samples remain available, and the
apparent speedups must not be presented as clean-machine estimates.

Evidence: `mac-fleet-arm-negative-{t2,tree,l9,aes,rss,zeros}-controls.json.zst`.
These files retain every sample, load observation, source hash and executable
identity. The normal macOS ARM build now selects this kernel. Ordered output
is enabled there only for `SeqOutStream::supports_streamed_blocks` sinks.
That capability defaults to false: the first promotion attempt exposed a
legacy `write_vec` ownership regression, fixed without weakening its test.
Existing custom sinks still receive complete blocks; sevenz's byte bridges
explicitly opt in. All 239 library tests and 17 sevenz bridge tests pass.
The ordinary product binary (without `kernel-ab`) passes all five affected
gates against both the frozen diagnostic candidate and 7zz. Three measured
repeats follow one warmup; all 60 operations pass archive integrity, with
identical plaintext product bytes and unchanged encrypted sizes.

| Ordinary product gate | Wall vs 7zz | CPU vs 7zz | RSS vs 7zz |
| --- | ---: | ---: | ---: |
| Solid tree L5 full threads | -8.45% | -5.04% | -1.62% |
| L5 T4 | -9.73% | -5.31% | +0.04% |
| L5 T8 | -10.84% | -5.39% | +0.06% |
| AES L5 full threads | -9.81% | -4.76% | -2.53% |
| L9 full threads | -9.49% | -5.71% | +0.02% |

The largest ordinary-product timing increase against the frozen diagnostic
candidate is L9 wall +1.07%, within the 3% limit. This checkpoint also includes
background compiler interference (826 recorded observations); it confirms
the measured gates, not universal architecture parity or every-run bounds.
`mac-fleet-arm-negative-product-controls.json.zst` preserves the complete
checkpoint, failed/passing correctness-log hashes, source identities and
normal-versus-diagnostic binary hashes. No prior failed cohort was removed.
The final integration checks also pass: four encoder allocation-failure tests
and 22 XZ encoder tests, including external xz acceptance and filter chains.
Formatting/Clippy and end-of-arc Miri remain deferred; neither is claimed here.

### L1 full-thread small output-slot reuse

A focused ordinary-product control confirmed the remaining L1 RSS excess:
301,826,048 bytes versus 288,292,864 for 7zz (+4.69%), despite 15.01% lower
wall and 15.68% lower CPU. Paired live heap snapshots show identical major
finder/input allocation sizes. The product has fewer live output allocations,
but retains 17.8 MiB in empty resident allocator mappings. The SDK retains its
per-slot output buffers until stream end rather than cycling them through free
and calloc. A documented `MADV_ZERO` experiment passed the allocation test but
saved no median RSS; it was removed and its full failed evidence preserved.

The candidate preserves the SDK's output-slot reuse for Darwin allocations up
to 8 MiB, retaining the existing reclamation of larger surplus buffers. L1 now
uses 291,110,912 bytes versus 288,309,248 (+0.97%), saving 10,190,848 bytes from
the frozen product. Wall is 15.49% lower and CPU 16.86% lower than 7zz, with
identical product archive hashes. Eighteen output-related tests pass, including
small-slot address reuse, pending-byte preservation and large-slot release.

The first T4/T8 preservation cohort includes competing compilers and fails
some baseline thresholds; T8 also exceeds reference RSS by 4.15%. These
failures remain evidence, not waived results. A paired T8 heap audit shows
matching live allocation sizes and another empty-mapping difference. Focused
follow-up validation is required before claiming the large-gate gains remain
closed. Evidence: `mac-fleet-l1-tall-product-controls.json.zst`,
`mac-fleet-l1-zero-reclaim-controls.json.zst`, and
`mac-fleet-l1-small-slot-reuse-controls.json.zst`.

### Explicit ownership of large output mappings

The T8 follow-up reproduced the excess in the frozen product as well as the
small-slot candidate. A bounded one-spare transfer experiment preserved time
but still exceeded reference RSS by 4.83%; it was removed and its evidence
retained. The working fix uses exclusively owned anonymous Darwin mappings
for large output buffers, only on the opted-in streamed-byte path. Retiring
a surplus buffer unmaps its pages directly. The SDK's block contents, coder
concurrency, ordered writer and admission bounds are unchanged. This storage
policy is an explicitly measured extension, not an upstream allocator port.

Mappings are never converted to Vecs. Legacy ownership-taking sinks retain
their existing buffers and delivery path. Bounded writes prevent copying
bridges from making a second whole-block allocation. Other platforms and
Miri retain the allocator-backed path. Native tests cover zero initialization,
page-edge sizes, rejected sizes, thread ownership, pending storage, reuse,
write failure and encoder byte parity. All 242 library tests and 26 sevenz
writer/bridge tests pass. End-of-arc Miri and capstone linting remain deferred.

| Focused gate | Wall vs 7zz | CPU vs 7zz | RSS vs 7zz | RSS vs frozen product |
| --- | ---: | ---: | ---: | ---: |
| L5 T4 | -9.35% | -5.08% | +1.75% | +1.60% |
| L5 T8 | -9.16% | -3.08% | -3.69% | +0.84% |

Both gates pass all three thresholds against both controls. T8 candidate RSS
is 619,724,800 bytes; the reference median in that cohort is 643,465,216,
with a lower reference sample of 614,137,856 also retained. The candidate is
still within 1% of that lower sample. T4 uses 314,245,120 versus 308,854,784
bytes. All product archives remain byte-identical. Samples and interference
observations are retained in `mac-fleet-mapped-output-{t4,t8}-controls.json.zst`;
the failed large-slot, T8 follow-up and one-spare cohorts remain separate.


### Final checkpoint scope

The operator accepted the indexed one-worker XZ verified-output buffer:
9.7–17.7 MiB above xz, with approximately 37–38% lower wall and CPU.
It is bounded by the checked block size and memory limit, doubles as the
dictionary, and preserves checksum validation before block delivery.
This is a scoped acceptance, not an unbounded-memory allowance.

The final L1 full-thread mapped-output control passes: RSS 291,127,296
versus 288,342,016 bytes (+0.97%), wall -15.05%, CPU -15.93%.

The operator accepted the tiny plain non-solid full-thread CPU tradeoff on
2026-10-15 for its 4.62x wall-time improvement. Separate admission,
consumer and capacity notifications avoid waking unrelated waiters; worker-local
coders retain reuse without a shared pool lock. Final measured medians are
0.075205 seconds wall and 0.501194 seconds CPU versus 7zz's 0.347526 and
0.436541 seconds: 4.62x faster wall, but CPU +14.81%. RSS is 26,755,072
versus 9,994,240 bytes. Against the frozen product, wall improves 12.56%
and CPU 17.44%; RSS rises 1,474,560 bytes. The CPU overage is an explicit
scoped acceptance, not a numeric threshold pass. RSS is bounded by the
requested worker count and falls under the accepted large-wall-win policy.
