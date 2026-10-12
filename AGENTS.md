# Agent and contributor rules for lzma-turbo

These rules apply to every automated agent and every human contributor.

## What this repository is

An LZMA/LZMA2 library ported from Igor Pavlov's public-domain SDK: the decoder
from `LzmaDec.c` / `Lzma2Dec.c`, and the encoder from `LzFind.c`, `LzmaEnc.c`
and `Lzma2Enc.c`. The point of the crate is speed with parity to the
reference — byte-identical output from the decoder, and bit-exact output from
the encoder. Readability that costs throughput is not wanted. The encoder's
port rules and what was left out are in `docs/encoder.md`.

## Porting rules

1. Port, do not redesign. Keep the reference decoder's structure: one decode
   loop, coder state in locals, per-symbol limit checks under a margin, a
   separate slow path near buffer edges. See `docs/porting.md`.
2. The reference is `C/` in a checkout of github.com/ip7z/7zip at the commit
   named in `docs/porting.md`. Cite the C function name in a comment when a Rust
   function corresponds to one.
3. `unsafe` is allowed where the C relies on the margin invariant, and
   elsewhere only with the verification specified below. Every unsafe block
   carries a `// SAFETY:` comment identifying its invariant and establishing
   check. Portable encoder paths have checked differential tests in the
   ordinary native suite and the required `unchecked-kernel-miri` CI job.
   Miri results do not verify assembly or OS FFI. The encoder inventory is:

   - Tree/table pointer access and the portable register walk:
     `the_unchecked_walk_is_the_checked_walk_on_any_tables`,
     `the_walk_refuses_what_it_cannot_vouch_for`, and the five
     `the_register_walk_*` tests in `src/enc/lz_find_mt.rs`. Native and Miri;
     Miri exercises the portable walk and its entry-margin wrapper.
   - x86-64 `sdk_walk` in `src/enc/bt_kernel_x86_64.S`: native-only; the
     production encoder selects it with `asm` enabled. The five
     `the_register_walk_*` tests also invoke it directly and compare answers,
     mutated trees and output against the checked walk, including invalid
     margins, short output, SDK modes/error order, and every word/tail.
     Miri never executes this assembly.
   - `zeroed_vec`: `a_zeroed_vec_is_zeroes_of_every_zeroable_type` in
     `src/enc/lz_find.rs`, native and Miri.
   - Shared finder storage and borrowed direct-input lifetime/ownership in
     `run_block`: `a_direct_block_is_read_as_the_copied_window_is` in
     `src/enc/lz_find_mt.rs`, native and Miri; native producer panic,
     cancellation and reuse tests cover the threaded lifecycle as well.
   - Low-hash/table access in `mix_matches_kind`:
     `the_mix_matches_kernel_matches_checked_tables`, native and Miri.
   - `EncodeWindow` byte/word reads and parser `match_word!` / `opt!`
     indexing: `encoder_window_reads_match_the_checked_parser` in
     `src/enc/lzma_enc.rs`, native and Miri, across finder families, parser
     modes, windows and tails. `the_window_match_lengths_equal_the_bytewise_oracle`
     also checks match lengths in the native suite.
   - `Finder::*_for` variant assumptions (`unreachable_unchecked`): the same
     checked-parser differential test exercises the single-threaded,
     inline and threaded variants natively and under Miri. `MT` is selected
     at block entry and the finder variant cannot change within the block.
   - `madvise_hugepage`: `the_advice_covers_only_whole_huge_pages_inside_the_buffer`
     checks the range, and `the_kernel_takes_the_advice_and_the_bytes_are_unchanged`
     checks the real FFI call natively on Linux. Miri has no `madvise`.
4. No dependencies in the decoder modules: `src/lzma/`, `src/lzma2/` and
   everything they reach must build from `core` and `alloc` alone. The only
   library dependencies the crate may take are `crc-fast`, behind the `crc`
   feature, and the SHA-256 backends — `aws-lc-rs` behind `crypto`, `sha2`
   behind `native-crypto` — all optional, all confined to `src/crc.rs` and
   `src/crypto/`, and none of them reachable from the decoder even when the
   default features have them on. Anything else needs a decision
   from the maintainer, not from an agent. Dev-dependencies and the bench tool
   may pull what they need.
5. Correctness is proven differentially: decoded bytes must equal what
   `xz -dc` / `7zz` produce for the same input, byte for byte, and malformed
   input must return an error rather than panic or read out of bounds.

## Performance rules

- The acceptance gate is `docs/benchmarking.md`. A change that regresses the
  gate does not merge, however clean it is.
- Measure with `cargo run --release -p lzma-bench`, on an otherwise idle
  machine, at least three runs, report the median.
- Never commit fixture bytes. `cargo xtask fixtures` regenerates them.

## Repository hygiene

- Commits are SSH-signed. Never pass `--no-gpg-sign`.
- Never run destructive git working-tree operations (`checkout --`,
  `restore`, `reset --hard`, `clean`, `stash`) in a shared checkout.
- Branch names use gitflow prefixes: `feature/…`, `bugfix/…`, `hotfix/…`.
- Do not push. The maintainer pushes.
- The pre-commit hook (`.githooks/pre-commit`) rejects home paths, the local
  username and secrets. Keep it enabled: `git config core.hooksPath .githooks`.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`
  must pass before a commit is proposed for review.
- Any code change to the crate (`src/`) bumps its version, `Cargo.lock` and
  `CHANGELOG.md` in the same change.
