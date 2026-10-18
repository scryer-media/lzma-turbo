# Benchmarks

Current measurements of lzma-turbo 0.10.1 against xz 5.8.4, 7-Zip 26.04,
lzma-rust2 0.20.1 and liblzma (the `liblzma` crate 0.4.8 over XZ Utils) on
two CPUs. Taken 2026-10-18 with the fleet harness in
[`bench/lzma-turbo-bench`](../bench/lzma-turbo-bench) (`--profile fleet`,
3 timed repeats after 1 warm-up, medians, 256 MiB payloads). How to reproduce
a measurement is in [benchmarking.md](benchmarking.md); the 0.9.0 encoder
audit with its retained samples is in
[encoder-perf-0.9.0.md](encoder-perf-0.9.0.md). Earlier results, including
the five-machine 0.9.0 round of 2026-10-13, are in this file's git history.

Every ratio is **reference / lzma-turbo**, so above 1.000 means lzma-turbo
took less wall time, less CPU time or less peak memory than the reference on
that row. Values below 0.970 are in bold. A dash means the reference does not
run that row (xz has no `.7z` or LZMA1 path, 7-Zip has no raw xz filter path)
or the row was not run on that host. `t1`, `t2`, `t4`, `t8` are fixed thread
counts, `tall` is every hardware thread the host has; `p1`, `p5`, `p9` are
presets. Encode rows carry a fourth column, size: reference output bytes /
lzma-turbo output bytes at the same settings. It is 1.000 on every encode
row on every host (0.9997 to 1.0004 before rounding), so the speedups are at
the same compression ratio.

Build: `lzma-turbo` 0.10.1 default features (`asm` loops, `crc-fast` 1.10.0,
`aws-lc-rs` 1.18.1 SHA-256), release profile, rustc 1.97.1. xz and 7-Zip
are the official 5.8.4 and 26.04 binaries for each platform.

## Hosts

| column | CPU | threads | OS | 7-Zip build |
| --- | --- | ---: | --- | --- |
| Sapphire Rapids | Intel Xeon Platinum 8488C (Sapphire Rapids, AVX-512) | 16 | Linux, c7i.4xlarge | 7-Zip (z) 26.04 (x64) |
| Graviton4 | AWS Graviton4 (Neoverse V2, SVE2) | 16 | Linux, c8g.4xlarge | 7-Zip (z) 26.04 (arm64) |

Both hosts were idle.

## Summary

Geometric mean of the per-row ratios on each host, over every row that
reference runs except `decode/lzma2-raw/p256` (see below).

| CPU | threads | vs xz: decode wall | encode wall | peak RSS | vs 7-Zip: decode wall | peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Sapphire Rapids | 16 | 1.10 | 1.85 | 0.89 | 1.00 | 1.10 |
| Graviton4 | 16 | 1.81 | 1.82 | 0.89 | 1.00 | 1.10 |

## lzma-turbo vs xz

`decode/lzma2-raw/p256` is not like for like: lzma-turbo decodes the bare
LZMA2 stream while xz decodes the whole `.xz` container around it, framing
and check included. It is shown but left out of the summary.

### decode-lzma1

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/lzma1/p256` | 1.049 | 1.049 | **0.867** | 1.830 | 1.830 | **0.864** |

### decode-xz

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/xz/p256` | 1.053 | 1.053 | **0.924** | 1.893 | 1.893 | **0.925** |
| `decode/lzma2-raw/p256` | 1.053 | 1.053 | **0.951** | 1.897 | 1.897 | **0.947** |
| `decode/xz-multi/p256x3` | 1.053 | 1.053 | **0.921** | 1.894 | 1.894 | **0.921** |

### decode-xz-par

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/xz-par/p256.t8/t1` | 1.053 | 1.053 | **0.917** | 1.890 | 1.890 | **0.921** |
| `decode/xz-par/p256.t8/tall` | 1.015 | 1.061 | 1.101 | 1.750 | 1.882 | 1.133 |
| `decode/xz-par/p256.b16/t1` | 1.055 | 1.055 | **0.906** | 1.889 | 1.889 | **0.919** |
| `decode/xz-par/p256.b16/tall` | 1.023 | 1.068 | 1.180 | 1.674 | 1.877 | 1.225 |
| `decode/xz-par/1g.t8/tall` | 1.042 | 1.066 | 1.299 | 1.832 | 1.899 | 1.303 |

### encode

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Sapphire Rapids size | Graviton4 wall | Graviton4 CPU | Graviton4 RSS | Graviton4 size |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `encode/st/p1` | 1.669 | 1.662 | **0.711** | 1.000 | 1.548 | 1.547 | **0.702** | 1.000 |
| `encode/st/p5` | 2.886 | 2.875 | **0.932** | 1.000 | 2.740 | 2.737 | **0.932** | 1.000 |
| `encode/st/p9` | 2.985 | 2.978 | **0.950** | 1.000 | 2.604 | 2.603 | **0.950** | 1.000 |

### encode-mt

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Sapphire Rapids size | Graviton4 wall | Graviton4 CPU | Graviton4 RSS | Graviton4 size |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `encode/mt/p5/t2` | 2.759 | 2.777 | 1.155 | 1.000 | 2.685 | 2.688 | 1.156 | 1.000 |
| `encode/mt/p5/t4` | 2.768 | 2.818 | 1.160 | 1.000 | 2.633 | 2.654 | 1.161 | 1.000 |
| `encode/mt/p5/t8` | 2.773 | 2.837 | 1.135 | 1.000 | 2.668 | 2.672 | 1.103 | 1.000 |
| `encode/mt/p5/tall` | 2.345 | 2.552 | 1.084 | 1.000 | 2.631 | 2.647 | 1.083 | 1.000 |

### filter

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Sapphire Rapids size | Graviton4 wall | Graviton4 CPU | Graviton4 RSS | Graviton4 size |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `filter/decode/x86` | 1.183 | 1.184 | **0.930** | — | 1.550 | 1.550 | **0.925** | — |
| `filter/encode/x86` | 1.452 | 1.429 | **0.704** | 1.000 | 1.559 | 1.545 | **0.693** | 1.000 |
| `filter/decode/arm64` | 0.995 | 0.995 | **0.930** | — | 1.416 | 1.416 | **0.926** | — |
| `filter/encode/arm64` | 1.415 | 1.389 | **0.703** | 1.000 | 1.368 | 1.354 | **0.694** | 1.000 |
| `filter/decode/arm` | 0.998 | 0.998 | **0.928** | — | 1.449 | 1.450 | **0.925** | — |
| `filter/encode/arm` | 1.419 | 1.400 | **0.704** | 1.000 | 1.589 | 1.575 | **0.694** | 1.000 |
| `filter/decode/armthumb` | 0.987 | 0.986 | **0.926** | — | 1.415 | 1.415 | **0.932** | — |
| `filter/encode/armthumb` | 1.406 | 1.377 | **0.703** | 1.000 | 1.380 | 1.362 | **0.698** | 1.000 |
| `filter/decode/ppc` | 1.021 | 1.021 | **0.930** | — | 1.443 | 1.443 | **0.927** | — |
| `filter/encode/ppc` | 1.402 | 1.383 | **0.704** | 1.000 | 1.473 | 1.462 | **0.694** | 1.000 |
| `filter/decode/sparc` | 0.995 | 0.994 | **0.930** | — | 1.438 | 1.438 | **0.925** | — |
| `filter/encode/sparc` | 1.418 | 1.396 | **0.703** | 1.000 | 1.396 | 1.386 | **0.694** | 1.000 |
| `filter/decode/ia64` | 1.004 | 1.004 | **0.927** | — | 1.439 | 1.439 | **0.925** | — |
| `filter/encode/ia64` | 1.404 | 1.378 | **0.702** | 1.000 | 1.432 | 1.419 | **0.694** | 1.000 |
| `filter/decode/riscv` | 1.009 | 1.009 | **0.929** | — | 1.425 | 1.425 | **0.925** | — |
| `filter/encode/riscv` | 1.417 | 1.389 | **0.701** | 1.000 | 1.460 | 1.443 | **0.694** | 1.000 |
| `filter/decode/delta4` | 1.083 | 1.083 | **0.918** | — | 1.871 | 1.870 | **0.925** | — |
| `filter/encode/delta4` | 1.772 | 1.755 | **0.696** | 1.000 | 1.577 | 1.572 | **0.694** | 1.000 |
| `filter/decode/delta64` | 3.234 | 3.255 | **0.929** | — | 8.136 | 8.143 | **0.925** | — |
| `filter/encode/delta64` | 1.753 | 1.736 | **0.705** | 1.000 | 1.527 | 1.522 | **0.694** | 1.000 |

### check

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `check/none` | 1.053 | 1.053 | **0.919** | 1.845 | 1.845 | **0.925** |
| `check/crc32` | 1.053 | 1.053 | **0.924** | 1.844 | 1.844 | **0.925** |
| `check/crc64` | 1.052 | 1.052 | **0.933** | 1.894 | 1.894 | **0.925** |
| `check/sha256` | 1.144 | 1.144 | **0.930** | 1.928 | 1.928 | **0.925** |

## lzma-turbo vs 7-Zip

### decode-lzma1

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/lzma1/p256` | 0.997 | 0.997 | 1.182 | 1.000 | 1.000 | 1.178 |

### decode-xz

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/xz/p256` | 1.014 | 1.014 | 1.309 | 1.023 | 1.023 | 1.284 |

### decode-7z

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/7z-st/p256` | 1.011 | 1.011 | 1.096 | 1.008 | 1.008 | 1.092 |
| `decode/7z-st-fallback/p256/tall` | 1.007 | 1.007 | 1.009 | 1.004 | 1.004 | 1.009 |

### decode-7z-mt

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/7z-mt/p256/t1` | 1.010 | 1.010 | 1.044 | 1.003 | 1.003 | 1.046 |
| `decode/7z-mt/p256/tall` | 0.984 | 1.004 | 1.002 | 0.990 | 1.003 | 1.001 |

## lzma-turbo vs lzma-rust2

### decode-7z-mt

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/7z-mt/p256/t1` | 1.455 | 1.456 | 14.5 | 1.487 | 1.496 | 14.8 |
| `decode/7z-mt/p256/tall` | 2.713 | 1.411 | 1.090 | 1.439 | 1.466 | 1.352 |

## lzma-turbo vs liblzma

### decode-xz-par

| scenario | Sapphire Rapids wall | Sapphire Rapids CPU | Sapphire Rapids RSS | Graviton4 wall | Graviton4 CPU | Graviton4 RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `decode/xz-par/p256.t8/t1` | 1.088 | 1.088 | 1.059 | 1.915 | 1.915 | 1.042 |
| `decode/xz-par/p256.t8/tall` | 1.040 | 1.088 | 1.102 | 1.759 | 1.901 | 1.138 |
| `decode/xz-par/p256.b16/t1` | 1.087 | 1.087 | 1.028 | 1.913 | 1.913 | 1.042 |
| `decode/xz-par/p256.b16/tall` | 1.040 | 1.097 | 1.189 | 1.630 | 1.893 | 1.234 |
| `decode/xz-par/1g.t8/tall` | 1.064 | 1.094 | 1.217 | 1.839 | 1.916 | 1.299 |

## Raw filter throughput

The BCJ, BCJ2 and delta filters on their own, MiB/s of lzma-turbo on each
host, single thread. No reference runs a filter without a coder behind it,
so these rows have no ratio.

| scenario | Sapphire Rapids | Graviton4 |
| --- | ---: | ---: |
| `filter/raw/x86/encode` | 5408 | 6253 |
| `filter/raw/x86/decode` | 5881 | 6353 |
| `filter/raw/arm64/encode` | 4118 | 4492 |
| `filter/raw/arm64/decode` | 4277 | 4834 |
| `filter/raw/arm/encode` | 6675 | 7369 |
| `filter/raw/arm/decode` | 6732 | 8380 |
| `filter/raw/armthumb/encode` | 3297 | 3361 |
| `filter/raw/armthumb/decode` | 3325 | 3347 |
| `filter/raw/ppc/encode` | 6300 | 9587 |
| `filter/raw/ppc/decode` | 9926 | 9716 |
| `filter/raw/sparc/encode` | 6426 | 7923 |
| `filter/raw/sparc/decode` | 6166 | 7496 |
| `filter/raw/ia64/encode` | 3749 | 5705 |
| `filter/raw/ia64/decode` | 3673 | 5766 |
| `filter/raw/riscv/encode` | 3635 | 3503 |
| `filter/raw/riscv/decode` | 3561 | 3523 |
| `filter/raw/delta4/encode` | 1704 | 1740 |
| `filter/raw/delta4/decode` | 4569 | 3885 |
| `filter/raw/delta64/encode` | 1704 | 1737 |
| `filter/raw/delta64/decode` | 7260 | 17026 |
| `filter/raw/bcj2/encode` | 716 | 781 |
| `filter/raw/bcj2/decode` | 1154 | 1264 |

## Lane view

This image is from the earlier 0.9.0 round on five machines and has not been
redrawn for this run. It groups that run by the rows that were still behind
parity, one lane per gap, against the primary reference for each row (7-Zip
for `.7z` and `.lzma` rows, xz otherwise). Click the image for the full-size
version.

[![lzma-turbo 0.9.0 vs primary reference, lane view](images/fleet-lanes.webp)](images/fleet-lanes.webp)
