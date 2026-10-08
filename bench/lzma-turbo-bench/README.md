# lzma-turbo-bench

A benchmark harness for running lzma-turbo against its oracles on any
machine, and for merging the results from many machines into one report.
Each measurement runs as its own process: lzma-turbo, lzma-rust2 and liblzma
through `lzma-bench --shot`, and `xz`, `7zz` and `7lzma` directly. Every row
therefore records that process's peak resident set, read from the kernel the
same way for every contender, beside its wall time and CPU time.

It is a Go module with one binary and only one dependency
(`golang.org/x/sys`, for the CPU feature flags). It is not published, and the
crate does not depend on it.

## Build

```sh
# the Rust side: tools/lzma-bench, release profile
cargo build --release --locked -p lzma-bench

# the harness (Go 1.26 or newer)
cd bench/lzma-turbo-bench
go build -o ../../target/lzma-turbo-bench .
```

`lzma-turbo-bench toolchain --build` runs the cargo build for you. The
harness finds the checkout by walking up from the working directory, or from
its own binary, so `target/lzma-turbo-bench` works from anywhere inside the
repository. Pass `--repo` from anywhere else.

## Oracles by OS

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| `xz` | XZ Utils on `PATH`; `.github/scripts/install-xz.sh` builds the pinned 5.8.3 the tests use | `brew install xz`, or the same script | the project's release `xz.exe` on `PATH` or in `LZMA_TURBO_XZ` (the script fetches it under Git Bash) |
| 7-Zip | `toolchain --fetch-7zz` (runs `cargo xtask sevenzip`: the pinned official 26.03 `7zz` into `target/sevenzip`), or `7zz` on `PATH` | the same, or `brew install sevenzip` | the same, which installs the official `7za.exe`; a `7z.exe`/`7za.exe` on `PATH` or in `C:\Program Files\7-Zip` also works |
| `7lzma` | optional: the LZMA SDK's `C/Util/Lzma`, built with `make -f makefile.gcc`, on `PATH` or in `LZMA_TURBO_7LZMA` | same | same, or leave it out |
| `tar` | needed by `cargo xtask fixtures` | built in | built into Windows 10 and later |
| peak RSS | `getrusage` `ru_maxrss` (KiB) | `ru_maxrss` (bytes) | `PeakWorkingSetSize` from `K32GetProcessMemoryInfo` |

On Linux and macOS a `7z` on `PATH` is usually p7zip, a fork that stopped at
16.02, so only a binary named `7zz` is accepted there. Any oracle the host
lacks is left out of the rows that use it, and the report lists it with the
reason. A row with no reference left is skipped with that reason rather than
failed. lzma-rust2 and liblzma are compiled into `lzma-bench` at the versions
in `Cargo.lock`, so they are on every host.

## Commands

```sh
lzma-turbo-bench toolchain [--build] [--fetch-7zz]
lzma-turbo-bench fixtures  [--verify-only] [--json]
lzma-turbo-bench run       [--profile quick|full|fleet] [--quick]
                           [--repeats N] [--warmups N] [--out DIR]
                           [--machine LABEL] [--threads 1,2,4,8,16,all]
                           [--presets 1,3,5,6,9] [--mt-presets 5,6]
                           [--sizes p256,1g] [--only PREFIX|GLOB,...]
                           [--timeout 60m] [--build] [--list]
lzma-turbo-bench report    --input raw.json [--out report.json] [--md report.md]
lzma-turbo-bench merge     [--out merged.md] host-a/report.json host-b/report.json ...
```

- `toolchain` prints what every row depends on, as JSON:
  - the crate version, commit and dirty flag, plus `rustc`, `cargo` and Go;
  - the `lzma-bench` binary's SHA-256 and what `lzma-bench --shot info` says
    about the build: the decode loop compiled in, and the CRC tier `crc-fast`
    picked on this CPU;
  - for each oracle, its path, version banner, SHA-256 and how it was found;
  - for `xz`, the filters it supports;
  - the `Cargo.lock` versions of lzma-rust2, liblzma, crc-fast and aws-lc-rs.
- `fixtures` runs `cargo xtask fixtures`, then builds the harness's extra
  fixtures. It checks every file against `bench/fixtures/README.md` and caches
  the digests in `bench/fixtures/manifest.json`.
- `run` resolves the toolchain, describes the host, and runs the matrix. It
  writes `runs.jsonl` as each process finishes, then `raw.json`,
  `report.json` and `report.md`. Without `--out` the files go to
  `bench/results/<machine>-<UTC time>/`, which is gitignored.
  - `--profile` picks the matrix; the explicit sweep flags then narrow or
    widen it. There are three profiles:
    - `quick` (the same as `--quick`) is the smoke test: the 256 MiB payload
      only, threads 1 and all, presets 1 and 5, MT preset 5, and one repeat
      with no warmup.
    - `full`, the default, is every sweep at both sizes, with five repeats
      and one warmup pass.
    - `fleet` is sized for one run per fleet host in about an hour and a
      half. It keeps every `quick` row and adds presets 1, 5 and 9 on one
      thread and a 2, 4, 8 and all-CPU multi-threaded encode sweep at preset
      5 only. It keeps one 1 GiB row, `decode/xz-par/1g.t8/tall`, as a check
      that the 256 MiB numbers hold at size. It runs three repeats and one
      warmup pass, 66 scenarios in all. On the hosts measured so far it
      takes about 45 minutes on an Apple M5 Max, 75 minutes on an i5-1240P
      under Linux and 70 minutes on a Ryzen 5 3600 under Windows. The
      single-thread preset 9 and 5 encodes take about 40% of that.
  - `--list` prints the scenario IDs the flags select, then the plan: the
    scenario count, the contender rows the report will have and the number
    of processes the run will launch. `run` logs the same plan line before
    it starts.
- `report` rebuilds `report.json` and `report.md` from a `raw.json`, so a
  change to the report needs no re-measurement.
- `merge` lays several hosts' `report.json` side by side in one Markdown
  report. It shows the hosts and their tiers, then three tables with one
  column per host: lzma-turbo's speedup over each scenario's primary
  reference, lzma-turbo's time, and lzma-turbo's peak RSS.

## The matrix

| scenario | input | lzma-turbo | references |
| --- | --- | --- | --- |
| `decode/lzma1/{p256,1g}` | `p256.bin.lzma`, `payload.bin.lzma` | `LzmaReader` | `7zz t -mmt=1`, `xz -dc -T1 --format=lzma`, `7lzma d` |
| `decode/xz/p256` | `p256.bin.xz` | `XzReader` | `xz -dc -T1`, `7zz t -mmt=1` |
| `decode/lzma2-raw/p256` | `p256.bin.xz` | `Lzma2Reader` on the bare stream | `xz -dc -T1` |
| `decode/xz-multi/p256x3` | `multi.xz` | `XzReader` | `xz -dc -T1` |
| `decode/7z-st/{p256,1g}` | `p256.st.7z`, `st.7z` | `Lzma2Reader` | `7zz t -mmt=1` |
| `decode/7z-st-fallback/{p256,1g}/tall` | same | `Lzma2ParallelDecoder`, every CPU (must fall back to one thread) | `7zz t -mmt=<all>` |
| `decode/7z-mt/{p256,1g}/t<N>` | `p256.mt.7z`, `mt.7z` | `Lzma2ParallelDecoder` | `7zz t -mmt=N`, lzma-rust2 `Lzma2ReaderMt` |
| `decode/xz-par/{p256.t8,p256.b16,1g.t8}/t<N>` | `p256.t8.xz`, `p256.b16.xz`, `payload.t8.xz` | `XzParallelReader` | `xz -dc -T<N>`, liblzma MT |
| `encode/st/p<P>` | `p256.bin` | `XzWriter`, one solid block | `xz -T1 -<P>`, with the output-size ratio |
| `encode/mt/p<P>/t<N>` | `p256.bin` | `XzWriter`, `xz -T` block size | `xz -T<N> -<P>`, with the output-size ratio |
| `filter/decode/<f>` | `bcj-<f>.code.xz`, `delta.xz`, `delta64.xz` | `XzReader` | `xz -dc -T1` |
| `filter/encode/<f>` | `codebin.bin`, `p256.bin` (delta) | `XzWriter` + filter, preset 1 | `xz -T1 --<f> --lzma2=preset=1` |
| `filter/raw/<f>/{encode,decode}` | same | the bare converter in memory, timed region only | none: no oracle runs a bare converter |
| `check/{none,crc32,crc64,sha256}` | `p256.{none,crc32,sha256}.xz`, `p256.bin.xz` | `XzReader` | `xz -dc -T1`; the report subtracts `none` to show what each check costs |

The filters `<f>` are BCJ x86, ARM64, ARM, ARM-Thumb, PowerPC, SPARC, IA-64
and RISC-V, plus delta at distances 4 and 64. BCJ2 appears only in
`filter/raw`, because xz has no BCJ2. A filter the local xz lacks is skipped,
with the reason. The thread sweep is 1, 2, 4, 8, 16 and all (every logical
CPU). A count above the host's CPU count is skipped rather than
oversubscribed, and `all` is dropped when it equals a count already in the
sweep. Encoding a single thread at presets 1, 3, 5, 6 and 9, and the MT
encode at presets 5 and 6, completes the defaults.

Every decode that lzma-turbo, lzma-rust2 or liblzma runs is checked: the
output's CRC-32 has to equal the source file's, or the run fails.
`xz`, `7zz` and `7lzma` check their own streams. A reference that exits
non-zero or times out is marked DNF and its remaining runs are skipped. When
lzma-turbo fails, or a run finishes without a peak RSS, the row fails.

## Reading the report

- Every ratio is reference / lzma-turbo, so **above 1.000 always means
  lzma-turbo is better**:
  - speed and CPU, from median seconds: lzma-turbo is faster or used less CPU;
  - RSS, from median peaks: lzma-turbo peaked lower;
  - size, from output bytes: lzma-turbo's stream is smaller.
- rarpar-bench's ratios run the other way (rarpar/reference, where below
  1.000 is better). This harness inverts every one of them so the fleet
  collector reads one direction everywhere; sevenz-turbo's harness uses the
  same convention.
- "Peak RSS per scenario" sorts worst first, which is the lowest ratio.
- Times are reported as median [min–max] over the measured runs. Contenders
  run interleaved, and the order reverses on alternate repeats. The load
  average read before each run is kept in `raw.json`, and its median per row
  in `report.json`.
- The ISA section states the facts that apply to every row on the host. The
  decode loop is fixed at compile time. `crc-fast` chooses the CRC tier at
  run time, and aws-lc-rs dispatches SHA-256 internally. The rest is
  portable Rust, so the tier difference between two hosts is in the CPU and
  in those two libraries.
- Peak RSS covers a process's own high-water mark. lzma-rust2 and liblzma
  run inside `lzma-bench`, so their figures include its small image.
  liblzma's C allocations do not reach the Rust heap counter, which leaves
  its "peak alloc" near zero. The bare-converter rows read their whole input
  into memory and say how much.
- `codebin.bin` is the host's own machine code. A BCJ filter for another
  architecture has little to convert in it.
- The compressed fixtures depend on the xz and 7-Zip that wrote them. For
  hosts to be comparable, use the same pinned versions everywhere; the
  toolchain record names them.

## Fleet use

```sh
# once per host
cargo build --release --locked -p lzma-bench
(cd bench/lzma-turbo-bench && go build -o ../../target/lzma-turbo-bench .)
target/lzma-turbo-bench toolchain --fetch-7zz > /dev/null
target/lzma-turbo-bench fixtures

# the measurement
target/lzma-turbo-bench run --out "$RESULTS/$LABEL" --machine "$LABEL"

# on the collector
target/lzma-turbo-bench merge --out fleet.md results/*/report.json
```

A rarpar-bench suite can wrap these as its macro step. The step's output
directory holds `raw.json`, `report.json` and `report.md`, plus the
`runs.jsonl` journal, and exit status 0 means a complete report. The host
descriptor in `report.json` follows rarpar-bench's: OS, kernel, architecture,
CPU model, logical CPU count, memory, ISA flags with their source, a tier and
the load source. The measurement field names (`wall_seconds`, `user_seconds`,
`sys_seconds`, `max_rss_bytes`, `rss_source`, `exit_code`) are also the
same. No hostname is recorded. The label is `--machine`, or
`<os>-<arch>-<cpus>cpu`.

### Environment

| variable | meaning |
| --- | --- |
| `LZMA_TURBO_BENCH_BIN` | the `lzma-bench` binary (default `target/release/lzma-bench`) |
| `LZMA_TURBO_XZ` | the `xz` to use instead of the one on `PATH` |
| `LZMA_TURBO_7ZZ` | the 7-Zip console binary to use, ahead of `target/sevenzip` and `PATH` |
| `LZMA_TURBO_7LZMA` | the LZMA SDK's `lzma` utility |

### Exit status

| code | meaning |
| --- | --- |
| 0 | every selected row ran or was skipped with a reason; no failures |
| 1 | a failure: lzma-turbo failed or produced the wrong bytes, a run had no peak RSS, a fixture is missing or wrong (`fixtures`), or the run was interrupted (the partial report is still written) |
| 2 | usage: an unknown command or flag, or `--only` matched nothing |
| 3 | the host is not ready: no lzma-turbo checkout, no `lzma-bench` build, or an unwritable output directory |
