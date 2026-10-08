# Benchmark fixtures

Generated locally; every file in this directory except this README is
gitignored. `cargo xtask fixtures` makes the first table (it needs `xz`,
`7zz` and `tar` on `PATH` and about 4.5 GiB free) and
`lzma-turbo-bench fixtures` (`bench/lzma-turbo-bench`) runs it, makes the
harness extras below, and checks every file against these tables. A fixture
that already exists is left alone.

Only the payloads are the same bytes on every machine: they come from a
seeded generator, so their SHA-256 is fixed. Every compressed fixture depends
on the xz or 7-Zip build that wrote it, so it is checked by size only (within
10% of the figure here). `codebin.bin` is the executables under this
checkout's `target/` concatenated, so it is this machine's own code and its
size and digest vary.

| File | Size | SHA-256 | How |
| --- | --- | --- | --- |
| `payload.bin` | 1073741824 | `5d6dea8564e6412763756bf5a3e05d15e40592f28ae6a8800e1d91c10a386658` | synthetic, seeded; compresses to about 0.88 |
| `p256.bin` | 268435456 | `1f01b30af4ef1142dc8fc0c9f7298028d029ea163b385e9d87b8ef15079498a6` | first 256 MiB of payload.bin |
| `p256.bin.lzma` | ~224 MiB | - | `xz -T1 -5 --format=lzma` (LZMA1) |
| `payload.bin.lzma` | ~897 MiB | - | same, on payload.bin |
| `p256.bin.xz` | ~224 MiB | - | `xz -T1 -5` (one LZMA2 block in an .xz) |
| `st.7z` | ~897 MiB | - | `7zz a -mx=5 -m0=lzma2 -mmt=1` on payload.bin (one LZMA2 stream) |
| `mt.7z` | ~897 MiB | - | `7zz a -mx=5 -m0=lzma2 -mmt=on` on payload.bin (multi-threaded chunking) |
| `p256.t8.xz` | ~225 MiB | - | `xz -T8 -5` (multi-block) |
| `p256.b16.xz` | ~225 MiB | - | `xz -T8 -5 --block-size=16MiB` |
| `payload.t8.xz` | ~898 MiB | - | `xz -T8 -5` on payload.bin |
| `bcj-x86.xz` | varies | - | `xz -T1 -5 --x86` on the local `xz` binary |
| `delta.xz` | ~235 MiB | - | `xz -T1 -5 --delta=dist=4` on p256.bin |
| `delta64.xz` | ~256 MiB | - | `xz -T1 -5 --delta=dist=64` on p256.bin |
| `codebin.bin` | up to 64 MiB | - | the executables under `target/`, largest first |
| `bcj-x86.code.xz` | varies | - | `xz -T1 -5 --x86` on codebin.bin |
| `bcj-arm64.code.xz` | varies | - | `xz -T1 -5 --arm64` on codebin.bin |
| `p256.sha256.xz` | ~224 MiB | - | `xz -T1 -5 --check=sha256` |
| `p256.crc32.xz` | ~224 MiB | - | `xz -T1 -5 --check=crc32` |
| `multi.xz` | ~673 MiB | - | three copies of p256.bin.xz concatenated |
| `tree.tar.xz` | varies | - | `tar` of src, tools, docs and xtask, `xz -T8 -5` |

## Harness extras

Made by `lzma-turbo-bench fixtures`, which writes each to a temporary name
and renames it into place. A branch filter the local xz lacks is skipped with
the reason, and so are the rows that need it.

| File | Size | SHA-256 | How |
| --- | --- | --- | --- |
| `p256.none.xz` | ~224 MiB | - | `xz -T1 -5 --check=none` (the check-cost baseline) |
| `p256.st.7z` | ~224 MiB | - | `7zz a -t7z -mx=5 -m0=lzma2 -mmt=1` on p256.bin |
| `p256.mt.7z` | ~224 MiB | - | `7zz a -t7z -mx=5 -m0=lzma2 -mmt=on` on p256.bin |
| `bcj-arm.code.xz` | varies | - | `xz -T1 -5 --arm` on codebin.bin |
| `bcj-armthumb.code.xz` | varies | - | `xz -T1 -5 --armthumb` on codebin.bin |
| `bcj-ppc.code.xz` | varies | - | `xz -T1 -5 --powerpc` on codebin.bin |
| `bcj-sparc.code.xz` | varies | - | `xz -T1 -5 --sparc` on codebin.bin |
| `bcj-ia64.code.xz` | varies | - | `xz -T1 -5 --ia64` on codebin.bin |
| `bcj-riscv.code.xz` | varies | - | `xz -T1 -5 --riscv` on codebin.bin |
