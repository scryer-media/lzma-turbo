# lzma-turbo

[![ci](https://github.com/scryer-media/lzma-turbo/actions/workflows/ci.yml/badge.svg)](https://github.com/scryer-media/lzma-turbo/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lzma-turbo.svg)](https://crates.io/crates/lzma-turbo)
[![docs.rs](https://docs.rs/lzma-turbo/badge.svg)](https://docs.rs/lzma-turbo)

LZMA and LZMA2 in Rust, ported from the 7-Zip reference implementation for its
speed, including its hand-written `aarch64` and `x86_64` decode loops. The
encoder is a port too, and is bit-exact with the SDK's. No C bindings, no
build script.

```toml
[dependencies]
lzma-turbo = "0.4"
```

## Reading an `.xz` file

```rust
use std::fs::File;
use std::io::Read;
use lzma_turbo::xz::XzReader;

fn main() -> std::io::Result<()> {
    let mut out = Vec::new();
    XzReader::new(File::open("archive.tar.xz")?)
        .with_memory_limit(128 << 20)
        .read_to_end(&mut out)?;
    Ok(())
}
```

A file that is seekable decodes block-parallel with `xz::XzParallelReader`,
and one that is still arriving decodes with `xz::XzAdaptiveDecoder`: input is
fed as it lands, output is drained as `(offset, bytes)`, and each block is
either handed whole to a worker or chased on the caller's thread depending on
whether all of it has arrived.

## Why another LZMA crate

Every pure-Rust LZMA decoder descends from the Tukaani XZ-for-Java design: a
readable object-oriented state machine that pays for per-bit method calls,
bounds checks and struct-resident coder state. 7-Zip's own decoder is a
different shape, one large loop with everything in registers and limits
checked once per symbol, and it decodes the same streams 1.3x to 1.75x faster
on a single core. This crate ports that shape rather than that lineage.

## Speed

Linux x86_64, Intel Arrow Lake-H, 16 threads, median of three runs. Every
row decodes the same bytes and is checked byte for byte against `xz`.

One thread:

| stream | lzma-turbo | `7zz -mmt=1` | `xz -T1` |
| --- | --- | --- | --- |
| 1 GiB LZMA1 | 19.6 s | 19.6 s | 20.5 s |
| 256 MiB LZMA2 (.xz) | 4.9 s | 5.1 s | 5.1 s |

LZMA2 in parallel, a 1 GiB stream written by `7zz -mmt=on`:

| threads | lzma-turbo | `7zz` | lzma-rust2 |
| --- | --- | --- | --- |
| 1 | 20.4 s | 20.5 s | 29.6 s |
| 4 | 6.5 s | 6.5 s | 29.3 s |
| 8 | 4.3 s | 3.9 s | 29.3 s |
| 16 | 4.2 s | 3.9 s | 29.0 s |

`.xz` in parallel, 256 MiB in 16 blocks:

| threads | `XzParallelReader` | `xz -T<n>` |
| --- | --- | --- |
| 1 | 5.0 s | 5.2 s |
| 4 | 1.6 s | 1.6 s |
| 16 | 0.60 s | 0.69 s |

The 8- and 16-thread LZMA2 rows are the one place `7zz` is ahead, by how
well its threads land on this machine's performance cores; pinned to those
cores the two are within 5%. The full tables, with peak memory and the macOS
and Windows rows, are in
[docs/perf-log.md](https://github.com/scryer-media/lzma-turbo/blob/main/docs/perf-log.md).

## Status

The decoder is real: LZMA1 and LZMA2, ported function by function
from the reference decoder. It is byte-identical to
`xz -dc` on the repository's fixtures (256 MiB LZMA1, 1 GiB LZMA1, 256 MiB
LZMA2) and on the committed vectors, including non-default `lc`/`lp`/`pb`, and
it is fuzzed for panics and out-of-bounds reads.

On `aarch64` and `x86_64` the inner loop is 7-Zip's own assembly, translated
into Rust inline assembly and selected by the default `asm` feature; turning
that feature off (`--no-default-features --features std`) falls back to the
portable Rust port of the C loop, which is what every other target uses. Both
paths are held to the same tests, and a differential test decodes every vector
with each and compares.

LZMA2 also decodes on several threads, behind the `std` feature: a port of
7-Zip's `Lzma2DecMt.c` over the `MtDec.c` ring of workers, cutting the stream
at the dictionary resets that make a run independently decodable. A stream
with no resets — what `7zz -mmt=1` produces — falls back to the
single-threaded decoder rather than buffering. Alongside it,
`Lzma2AdaptiveDecoder` decodes a stream that is still arriving: input is fed
rather than read, output is polled, and the thread count can be changed
mid-stream at run boundaries. See the "Adaptive use" section of
[docs/porting.md](https://github.com/scryer-media/lzma-turbo/blob/main/docs/porting.md).

Either threaded decoder will also checksum its own output, in the worker that
produced it rather than on the thread draining it — `Checksum::Crc32`,
`Crc64Xz` or `Sha256`, with a `ChecksumPlan` carrying the absolute offsets
where the consumer's own boundaries (a 7z folder's sub-streams, say) fall.
Each worker emits one CRC per piece of its block between those offsets, in one
pass, and `crc::CrcFolder` folds the pieces into any range the consumer asks
about without re-reading a byte. This is not a convenience: the ring's write
callback is its one serialised section, so a CRC computed by the consumer as
it receives the output costs the decode both its own time and the queueing it
induces on every other worker — measured at 16% of an eight-thread decode.
SHA-256 cannot be folded, so it is offered per whole block only, which is the
unit an xz stream checks.

The encoder is a port of the same SDK's `LzFind.c`, `LzmaEnc.c` and
`Lzma2Enc.c`, behind the default `enc` feature: the six match finders, the
optimal parser and the LZMA2 chunk layer, with `.lzma` and `.xz` writers and
`std::io::Write` adapters over them. The BCJ and delta filters are ported in
both directions too, so the `.xz` writer can emit filtered blocks and not only
plain LZMA2. BCJ2, 7z's four-stream branch converter, is ported in both
directions alongside them in `filters::bcj2`; xz has no filter id for it, so it
is not re-exported under `xz`. It is bit-exact with the reference encoder — the same input at
the same settings produces the same bytes, and every converter agrees with the
SDK's byte for byte in both directions — and the parity tests check that
against binaries built from the pinned SDK sources over a generated corpus.

It compresses in parallel as well, through a port of the SDK's `MtCoder.c` and
the multi-threaded paths of `Lzma2Enc.c`: give `Lzma2Encoder` or `XzEncoder` a
block size and a thread count and the blocks are compressed at once, in the
order the stream wants them. The thread count does not change the bytes — one
thread and sixteen produce the same stream at the same block size — and that is
checked against an SDK built without `Z7_ST`, which is the build that actually
threads. Inside a single block, `LzmaEncProps::with_num_threads(2)` runs the
SDK's threaded match finder (`LzFindMt.c`), a second thread behind one block;
unlike the block count, that setting can change the bytes, because the C's own
two builds differ there. Solid, single-threaded output stays the default. What
was left out, and how the frame around the compressed data is proved, is in
[docs/encoder.md](https://github.com/scryer-media/lzma-turbo/blob/main/docs/encoder.md).

Throughput work against the acceptance gate (within 3% of `7zz t -mmt=1` on
the same file and machine) is tracked in [docs/perf-log.md](https://github.com/scryer-media/lzma-turbo/blob/main/docs/perf-log.md);
see [docs/porting.md](https://github.com/scryer-media/lzma-turbo/blob/main/docs/porting.md) for the plan.

## Features

| feature | default | what it adds |
| --- | --- | --- |
| `std` | yes | the `std::io::Read` adapters and `std::error::Error` |
| `asm` | yes | 7-Zip's own decode loop on `aarch64` and `x86_64` |
| `crc` | yes | CRC-32 and CRC-64/XZ, from `crc-fast`, their `CrcFolder`, and worker-side checksums in the threaded decoders |
| `crypto` | yes | SHA-256, xz check type 10, from `aws-lc-rs` |
| `enc` | yes | the encoder: `LzmaEncoder`, `Lzma2Encoder`, `XzEncoder`, the `.lzma`/`.xz` writers and the `Write` adapters; implies `crc` |
| `filters` | yes | the BCJ, BCJ2 and delta converters, `filters::bcj`, `filters::bcj2` and `filters::delta`, on their own: `no_std`, no dependency |
| `xz` | yes | the `.xz` container: `xz::XzReader`, `XzParallelReader`, `XzAdaptiveDecoder`, the checks and the index; implies `std`, `crc` and `filters`, and re-exports the converters at `xz::bcj` and `xz::delta` |
| `native-crypto` | no | the same SHA-256 API over RustCrypto's `sha2`, taking precedence over `crypto` |
| `crc-host` | no | on `wasm32`, the same CRC API delegated to embedder-installed hooks; implies `crc`. Inert on native targets - see [wasm](#wasm) |
| `crypto-host` | no | on `wasm32`, the same SHA-256 API delegated to embedder-installed hooks; implies `native-crypto` and `std`. Inert on native targets |

This crate is LZMA, LZMA2 and the xz container, and nothing else: 7z archives
are handled by a fork of `sevenz-rust2` that depends on it.

The decoder itself has no dependencies under any combination of these; `crc`
and `crypto` exist for the xz layer described in
[docs/porting.md](https://github.com/scryer-media/lzma-turbo/blob/main/docs/porting.md), and nothing in `src/lzma/` or
`src/lzma2/` can reach them. `--no-default-features` builds as `no_std` +
`alloc` with none of them.

`crc` and `crypto` are on by default because every xz stream carries a check,
and a check is one of CRC-32, CRC-64/XZ or SHA-256. `crypto` builds AWS-LC,
which needs a C toolchain and CMake; a build that wants neither takes

```toml
lzma-turbo = { version = "0.4", default-features = false, features = ["std", "asm", "crc", "native-crypto"] }
```

which is pure Rust and works wherever the decoder does. `native-crypto` wins
over `crypto` when both are enabled, so turning it on is an opt-out that
another crate in the graph cannot undo; with both compiled, a test requires
the two backends to produce the same digests.

## wasm

The crate builds for `wasm32-unknown-unknown` with `--no-default-features` and
with `--no-default-features --features std,asm,crc,native-crypto,xz`, and CI
checks both. That is the whole of the decoder, the `.xz` container, the
filters and the checks; `asm` is accepted and inert there (wasm gets the
portable loop), and SHA-256 has to come from `native-crypto`, because the
default `crypto` feature builds AWS-LC's C, which wasm has no toolchain for.
The threaded decoders need real threads and are not part of that set.

### Letting the host do the checksums

That build computes its checks in the guest, and both libraries it uses for
them are there for instructions wasm does not have: `crc-fast` exists for the
carry-less multiply units, and `sha2` has no `sha256rnds2` or `sha256h` to
reach for either. The host almost certainly does have them.

Two features hand those primitives back to the embedding program:

| Feature | What leaves the guest |
| --- | --- |
| `crc-host` | CRC-32 and CRC-64/XZ - every check the `.xz` container carries, header CRCs included |
| `crypto-host` | SHA-256, the check type 10 hash |

```toml
lzma-turbo = { version = "0.3", default-features = false, features = [
    "std", "asm", "crc-host", "crypto-host", "xz",
] }
```

The public API does not change: `crc::Crc32`, `crc::Crc64Xz`, `crc::crc32`,
`crc::crc64_xz` and `crypto::Sha256` keep their types and their methods, so
every caller - the readers, the block checks, the multi-threaded checksum
planner - picks the delegation up with no change of its own. The features
engage on `wasm32` only: on a native target they are accepted and inert, so
feature unification in a mixed workspace cannot silently turn a native build
into a delegating one.

The embedder installs a set of plain `fn` pointers at start-up:

```rust,ignore
use lzma_turbo::hooks::{HostHashHooks, install_host_hash_hooks};

install_host_hash_hooks(HostHashHooks::new(
    crc32, crc64_xz,
    sha256_init, sha256_clone, sha256_update, sha256_finalize, sha256_drop,
));
```

What sits behind them - a raw wasm import, a component import, a host SDK call
- is the embedder's business; this crate depends on no runtime, SDK or
interface definition and never learns which it is. The contract, in short:

- **The CRCs are seeded resumes in the finalized domain.** `crc32(0, d)` is the
  ordinary checksum of `d`, `crc32(s, &[]) == s`, and
  `crc32(crc32(0, a), b) == crc32(0, a ++ b)`. Same for `crc64_xz`. That last
  property is what lets the streaming digests carry a plain integer. A host
  whose CRC library exposes only the running register seeds it with `!seed`.
  Checksum *folding* (`crc32_combine`) is never delegated: it is arithmetic on
  two integers, not a pass over data.
- **SHA-256 is a streaming state behind an opaque handle.** `sha256_init`
  opens one, `sha256_update` appends, `sha256_clone` duplicates it,
  `sha256_finalize` returns the digest and closes it, `sha256_drop` closes one
  that never gets a digest. Each handle this crate opens is closed exactly
  once, so a multi-gigabyte block is hashed as it decodes and is never
  buffered, and a host may treat a stale handle as a bug.
- **A missing or contract-violating hook panics**, naming the call the embedder
  skipped. There is no in-guest fallback: it would return correct bytes at
  exactly the speed the embedding exists to avoid.

`src/hooks.rs` states the contract in full.
[`examples/wasm_xz_conformance.rs`](examples/wasm_xz_conformance.rs) is a
complete reference embedding - a `wasm32-wasip1` guest declaring raw imports in
a `host` namespace - and
[`tests/wasm_host_conformance.rs`](tests/wasm_host_conformance.rs) is the
reference host for that ABI, built on `crc-fast` and `sha2`. CI runs it: an
`.xz` of each check type is decoded in the guest and the report has to equal
the native decoder's byte for byte, down to the failure text of a stream whose
stored check was corrupted.

## Platforms

Built and tested on macOS aarch64, Linux x86-64 and windows-msvc x86-64. The
Windows lane is built with `clang-cl` and links AWS-LC statically, both with
its assembly generated from source by NASM (`AWS_LC_SYS_PREBUILT_NASM=0`) and
through its prebuilt objects; the test suite is run there in the assembly
build and in the portable (`--no-default-features --features std,crc`) build,
and `docs/perf-log.md` carries its `.xz` numbers.

## How it is tested

Every change runs the whole of this, on macOS aarch64, Linux x86-64, Linux
aarch64 and windows-msvc x86-64 unless a stage says otherwise:

- **Parity with the reference.** The decoders against the SDK's C and assembly
  loops, and the encoders, the threaded match finder, the block coder and the
  BCJ/BCJ2/delta filters against binaries `cargo xtask lzma-util` builds from
  the pinned SDK sources — byte for byte, on all four platforms.
- **The same bytes everywhere.** `cargo xtask golden --check` compares a
  handful of inputs at fixed settings against SHA-256 digests committed in
  `tests/golden.manifest`, so a platform cannot drift on its own even if it
  drifts together with the C it is compared against on that machine.
- **External decoders, required.** A pinned `xz` and a pinned `7zz` are
  installed on all four platforms and must read what this crate writes;
  `LZMA_TURBO_XZ_REQUIRE` and `LZMA_TURBO_7ZZ_REQUIRE` turn a missing tool
  from a skip into a failure.
- **Memory safety.** The decoder and encoder test suites under Valgrind's
  memcheck on Linux, and guard-page tests — every buffer placed against an
  unmapped page, from both ends — so a one-byte overrun faults instead of
  landing in slack.
- **Data races.** The threaded match finder, the block coder and the threaded
  decoder under ThreadSanitizer on x86-64 Linux, with the standard library
  rebuilt instrumented.
- **Fuzzing.** `differential` (both decode loops against the SDK's, call for
  call), `encode_round_trip` (whatever the encoder writes, the decoders
  return) and `encode_differential` (this encoder's bytes against the SDK
  encoder's, at settings the fuzzer picks) — briefly on every change, half an
  hour a night, with every target built on every change so none can rot.
- **The feature graph.** `no_std`, the wasm lanes with and without the host
  hash hooks, the MSRV, the published package, and the assembly's provenance
  against the SDK's own sources.

## Repository layout

| Path | What |
| --- | --- |
| [`src`](src), [`tests`](tests), [`fuzz`](fuzz) | The library crate (published to crates.io), its tests and its fuzz targets. |
| [`tools/lzma-bench`](tools/lzma-bench) | Decode- and encode-throughput harness used for the acceptance gate. Not published. |
| [`tools/sdk-oracle`](tools/sdk-oracle), [`tools/sdk-encoder`](tools/sdk-encoder) | The pinned SDK's decoder and encoder, linked into the differential tests and fuzz targets. Not published. |
| [`docs/porting.md`](docs/porting.md) | Port rules, C-to-Rust file map, acceptance gate. |
| [`docs/encoder.md`](docs/encoder.md) | The encoder port: C-to-Rust file map, what was left out, how parity is proved. |
| [`docs/benchmarking.md`](docs/benchmarking.md) | Fixtures, oracles and how to reproduce a measurement. |
| [`docs/security.md`](docs/security.md) | Every limit the container layer enforces, and what each one stops. |
| [`docs/publishing.md`](docs/publishing.md) | Release checklist. |
| [`xtask`](xtask) | `cargo xtask`: the release checks, the fixture generator, the pre-commit hook and the asm-lab harness, in dependency-free Rust. Not published. |
| `cargo xtask fixtures` | Generates the benchmark fixtures locally. They are never committed. |

## Acknowledgements

This crate is a translation, and the people whose work it translates deserve
to be named first.

- **Igor Pavlov** designed LZMA and LZMA2 and wrote 7-Zip and the
  [LZMA SDK](https://www.7-zip.org/sdk.html). Every decoder in this crate is a
  port of his: `LzmaDec.c` and `Lzma2Dec.c`, the multi-threaded
  `Lzma2DecMt.c` over `MtDec.c`, and the `aarch64` and `x86_64` decode loops
  from the SDK's `Asm/` tree. The speed this crate is named for is the shape
  of his code, kept as faithfully as Rust allows, and he placed all of it in
  the public domain.
- **Lasse Collin** and the [Tukaani project](https://tukaani.org/xz/) wrote
  the `.xz` format specification and `xz`, which is the oracle every stream
  this crate decodes is checked against, byte for byte.
- The **XZ for Java** lineage of pure-Rust decoders, whose readability set
  the bar this crate measures itself against, and whose authors made LZMA in
  Rust normal long before this port existed.

## License

The decoder is derived from `C/LzmaDec.c`, `C/Lzma2Dec.c`, `C/Lzma2DecMt.c`,
`C/MtDec.c` and `Asm/` in the LZMA SDK by Igor Pavlov, which are in the
public domain. This crate's source is licensed GPL-3.0-or-later; see
[LICENSE](LICENSE).

## Contributing

See [CONTRIBUTING.md](.github/CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).
Security reports go through [SECURITY.md](SECURITY.md).
