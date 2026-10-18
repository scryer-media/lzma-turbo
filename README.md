# lzma-turbo

[![ci](https://github.com/scryer-media/lzma-turbo/actions/workflows/ci.yml/badge.svg)](https://github.com/scryer-media/lzma-turbo/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/lzma-turbo.svg)](https://crates.io/crates/lzma-turbo)
[![docs.rs](https://docs.rs/lzma-turbo/badge.svg)](https://docs.rs/lzma-turbo)

Fast LZMA, LZMA2 and `.xz` compression for Rust. It is a port of the code
inside 7-Zip, Igor Pavlov's LZMA SDK, hand-written assembly included, so it
decompresses as fast as 7-Zip does and compresses to exactly the bytes the
SDK's encoder writes.

The codec is pure Rust, with no C bindings and no build script. The one C
library in the default build is AWS-LC, for SHA-256 checks; the
`native-crypto` feature replaces it with pure Rust (see
[Features](#features)).

```toml
[dependencies]
lzma-turbo = "0.10"
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

Two more readers cover the other common cases:

- `xz::XzParallelReader` decodes a file you can seek in on several threads.
- `xz::XzAdaptiveDecoder` decodes a file that is still arriving: feed it input
  as it lands and collect the output as it is produced.

Every failure is a typed error (corrupt data, input that ends too soon, a
memory limit reached, and so on). The `io::Error` a reader returns carries
it, so `io::Error::downcast` gives it back.

## Speed

Measured with lzma-turbo 0.10.1 against xz 5.8.4 and 7-Zip 26.04 on two idle
machines, 2026-10-18. Each number is the reference's time or memory divided
by lzma-turbo's, averaged (geometric mean) over every test that reference
runs, so above 1.00 means lzma-turbo was faster or used less memory.

| CPU | threads | vs xz: decode | encode | peak memory | vs 7-Zip: decode | peak memory |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Sapphire Rapids | 16 | 1.10 | 1.85 | 0.89 | 1.00 | 1.10 |
| Graviton4 | 16 | 1.81 | 1.82 | 0.89 | 1.00 | 1.10 |

- Decoding matches 7-Zip's speed. Against xz it is level to 1.18x on
  Sapphire Rapids (3.2x through the 64-byte delta filter) and 1.4 to 1.9x
  on Graviton4.
- Compressing is 1.37 to 3.0x faster than xz at every preset, thread count
  and filter, and the output is the same size.
- Slower than xz: decoding through the arm, arm64, armthumb and sparc branch
  filters on Sapphire Rapids (0.987 to 0.998). Slower than 7-Zip: the
  all-threads `.7z` decode (0.98 to 0.99) and LZMA1 decoding on Sapphire
  Rapids (0.997).
- Memory: lzma-turbo uses less than 7-Zip on every test, and about 0.9x of
  xz's on single-threaded decoding and 0.7 to 0.95x when compressing on one thread.
  It uses less than xz when compressing on two or more threads and when
  decoding in parallel on every thread.

Every test on every machine, CPU time and the comparisons with lzma-rust2 and
liblzma are in [docs/benchmarks.md](docs/benchmarks.md). To measure your own
machine, use [`bench/lzma-turbo-bench`](bench/lzma-turbo-bench).

## What it does

**Decoding.** LZMA, LZMA2 and `.xz`. The output matches `xz -dc` byte for
byte on the test files (up to 1 GiB) and on the committed test vectors, and
the decoder is fuzzed against malformed input. On `aarch64` and `x86_64` it
runs 7-Zip's own assembly decode loop; other targets, and builds without the
`asm` feature, use a Rust port of the C loop. The two are tested against each
other.

**Decoding on several threads.** An LZMA2 or `.xz` stream decodes on several
threads when it was written in independent pieces, as 7-Zip and xz write
them when they compress on several threads. A stream written in one piece,
by `7zz -mmt=1` for example, decodes on one thread. The threaded decoders can
also compute CRC-32, CRC-64 or SHA-256 checksums on their worker threads, for
callers such as 7z readers that need one checksum per file.

**Encoding.** `.lzma`, LZMA2 and `.xz`, with `std::io::Write` adapters. The
same input at the same settings gives the same bytes as the LZMA SDK's
encoder, checked against binaries built from the SDK's source. Given a block
size and a thread count, the encoder compresses blocks in parallel, and the
thread count does not change the output. A second match-finder thread inside
one block (`LzmaEncProps::with_num_threads(2)`) is available too; as in
7-Zip, that one can change the output bytes.

**Filters.** The branch converters for x86, ARM, ARM Thumb, ARM64, PowerPC,
SPARC, IA-64 and RISC-V, BCJ2 and delta, in both directions. The `.xz`
writer can use the branch converters and delta. BCJ2 has no `.xz` filter id,
so it is only in `filters::bcj2`, for 7z.

**Not here:** 7z archives. Those are
[sevenz-turbo](https://github.com/scryer-media/sevenz-turbo), which builds
on this crate.

How the decoder was ported, including the adaptive decoder, is in
[docs/porting.md](docs/porting.md); the encoder is in
[docs/encoder.md](docs/encoder.md); the limits a hostile file runs into are
in [docs/security.md](docs/security.md).

## Features

| feature | default | what it adds |
| --- | --- | --- |
| `std` | yes | the `std::io` readers and writers |
| `asm` | yes | 7-Zip's assembly decode loops on `aarch64` and `x86_64`, and the encoder's match-finder assembly on `x86_64` and macOS `aarch64` |
| `crc` | yes | CRC-32 and CRC-64, from `crc-fast`; implies `std` |
| `crypto` | yes | SHA-256 from AWS-LC, which needs a C compiler; implies `std` |
| `enc` | yes | the encoder; implies `crc` |
| `filters` | yes, through `xz` | the BCJ, BCJ2 and delta converters on their own: `no_std`, no dependencies |
| `xz` | yes | the `.xz` format; implies `std`, `crc` and `filters` |
| `native-crypto` | no | SHA-256 in pure Rust (`sha2`) instead of AWS-LC; wins when both are on |
| `crc-host` | no | WebAssembly only: the host computes the CRCs (see [WebAssembly](#webassembly)) |
| `crypto-host` | no | WebAssembly only: the host computes SHA-256 |
| `kernel-ab` | no | benchmarking only: switch the optimised loops off at run time |
| `adaptive-stats` | no | debugging only: print the adaptive decoder's counters on drop |

A build without a C compiler takes

```toml
lzma-turbo = { version = "0.10", default-features = false, features = ["std", "asm", "crc", "native-crypto"] }
```

and is pure Rust. The LZMA and LZMA2 decoder itself uses no dependency at
all: `--no-default-features` builds it as `no_std` with `alloc`.

## WebAssembly

The crate builds for `wasm32-unknown-unknown` with `--no-default-features`,
and with `--no-default-features --features std,asm,crc,native-crypto,xz` for
the decoder, `.xz` and its checks; CI builds both. `asm` does nothing there.
The threaded decoders need real threads and are not available.

`crc-host` and `crypto-host` let the program that embeds the module compute
the CRCs and SHA-256, which is usually much faster than computing them inside
WebAssembly. The API does not change, and on a native target the two
features do nothing.

```toml
lzma-turbo = { version = "0.10", default-features = false, features = [
    "std", "asm", "crc-host", "crypto-host", "xz",
] }
```

The host installs plain function pointers at start-up:

```rust,ignore
use lzma_turbo::hooks::{HostHashHooks, install_host_hash_hooks};

install_host_hash_hooks(HostHashHooks::new(
    crc32, crc64_xz,
    sha256_init, sha256_clone, sha256_update, sha256_finalize, sha256_drop,
));
```

A missing hook panics with the name of the call it needed. The full contract
is in `src/hooks.rs`.
[`examples/wasm_xz_conformance.rs`](examples/wasm_xz_conformance.rs) is a
complete guest, and
[`tools/wasm-conformance`](tools/wasm-conformance/tests/wasm_host_conformance.rs)
is the matching host, which CI runs.

## Platforms

Tested on macOS aarch64, Linux x86-64, Linux aarch64 and Windows x86-64
(MSVC), with the assembly and without it. On Windows, AWS-LC uses prebuilt
NASM objects, so no assembler has to be installed.

## How it is tested

Every change runs all of this, on all four platforms unless noted:

- Byte-for-byte comparison with the LZMA SDK's decoders, encoders and
  filters, built from pinned sources.
- Encoder output checked against committed SHA-256 digests
  (`cargo xtask golden --check`), so no platform can drift on its own.
- Pinned builds of `xz` and `7zz` must read everything this crate writes.
- Valgrind on Linux, and guard-page tests that turn a one-byte overrun into a
  crash.
- ThreadSanitizer on the threaded code, on x86-64 Linux.
- Fuzzing: briefly on every change and for half an hour every night.
- `no_std`, WebAssembly, the minimum Rust version, the published package,
  and the assembly checked against the SDK's own source.

## Repository layout

| Path | What |
| --- | --- |
| [`src`](src), [`tests`](tests), [`fuzz`](fuzz) | The library (published to crates.io), its tests and its fuzz targets. |
| [`tools/lzma-bench`](tools/lzma-bench) | The decode and encode benchmark the acceptance gate uses. Not published. |
| [`bench/lzma-turbo-bench`](bench/lzma-turbo-bench) | The Go harness that compares this crate with xz, 7-Zip, 7lzma, lzma-rust2 and liblzma on any machine, with peak memory. Not published. |
| [`tools/sdk-oracle`](tools/sdk-oracle), [`tools/sdk-encoder`](tools/sdk-encoder) | The pinned SDK's decoder and encoder, for the comparison tests and fuzz targets. Not published. |
| [`docs/porting.md`](docs/porting.md) | How the decoder was ported, and the acceptance gate. |
| [`docs/encoder.md`](docs/encoder.md) | How the encoder was ported, what was left out, and how parity is proved. |
| [`docs/benchmarking.md`](docs/benchmarking.md) | Fixtures, reference tools and how to reproduce a measurement. |
| [`docs/benchmarks.md`](docs/benchmarks.md) | The latest results, every test on every machine. |
| [`docs/security.md`](docs/security.md) | Every limit the `.xz` reader enforces, and what each one stops. |
| [`docs/publishing.md`](docs/publishing.md) | How a release is made. |
| [`xtask`](xtask) | `cargo xtask`: release checks, fixture generation, the pre-commit hook and the asm-lab harness. Not published. |

`cargo xtask fixtures` generates the benchmark fixtures locally
([bench/fixtures/README.md](bench/fixtures/README.md)); they are never
committed.

## Acknowledgements

This crate is a translation, and the people whose work it translates come
first.

- **Igor Pavlov** designed LZMA and LZMA2 and wrote 7-Zip and the
  [LZMA SDK](https://www.7-zip.org/sdk.html). Every decoder and encoder here
  is a port of his code, and he placed all of it in the public domain.
- **Lasse Collin** and the [Tukaani project](https://tukaani.org/xz/) wrote
  the `.xz` format specification and `xz`, against which every stream this
  crate decodes is checked.
- The authors of the XZ for Java line of pure-Rust decoders made LZMA in
  Rust normal long before this port existed.

## License

The decoder and encoder derive from the LZMA SDK by Igor Pavlov, which is in
the public domain. This crate's source is licensed under the
[Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0); see
[LICENSE](LICENSE). Releases before 0.7.0 were published under
GPL-3.0-or-later and remain available under that license.

## Contributing

See [CONTRIBUTING.md](.github/CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).
Security reports go through [SECURITY.md](SECURITY.md).
