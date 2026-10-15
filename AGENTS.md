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
     `the_walk_refuses_what_it_cannot_vouch_for`, and the six
     `the_register_walk_*` tests in `src/enc/lz_find_mt.rs`. Native and Miri;
     Miri exercises the portable walk and its entry-margin wrapper, including
     unaligned whole-node copies and repeated nodes at each output boundary.
     ARM64 production builds with `asm` select this portable walk; it is Rust
     pointer code, covered by both native differential tests and Miri.
   - x86-64 assembly kernels: native-only. The fixed `asm` candidate selects
     `sdk_walk_faithful` in `src/enc/bt_kernel_x86_64_sdk.S`, the complete
     upstream expansion. The six
     `the_register_walk_*` tests also invoke it directly and compare answers,
     mutated trees and output against the checked walk, including invalid
     margins, short output, SDK modes/error order, and every word/tail.
     The earlier `sdk_walk` in `src/enc/bt_kernel_x86_64.S` remains available
     to native tests and the non-default `kernel-ab` feature.
     `sdk_walk_faithful_wide` in
     `src/enc/bt_kernel_x86_64_sdk_wide.S` retains that flow with the bounded
     match extension, also only for tests and `kernel-ab`. All six tests
     explicitly run all three variants through the same margin wrapper; no
     environment mutation is needed to select a test variant. Miri never
     executes assembly.
   - Shared decoder/encoder `zeroed_vec` in `src/buffer.rs`:
     `a_zeroed_vec_is_zeroes_of_every_zeroable_type` in `src/enc/lz_find.rs`
     exercises the common allocator through the encoder wrapper, native and
     Miri. `decoder_dictionary_allocation_failure_is_recoverable` checks both
     decoder constructors under allocation refusal natively. Huge-page advice
     remains encoder-only; the decoder allocation uses only `core` and `alloc`.
   - Shared finder storage and borrowed direct-input lifetime/ownership in
     `run_block`: `a_direct_block_is_read_as_the_copied_window_is` in
     `src/enc/lz_find_mt.rs`, native and Miri; native producer panic,
     cancellation and reuse tests cover the threaded lifecycle as well.
     `a_streamed_window_is_counted_when_reusing_a_block_coder` also checks
     retained-window accounting between streams, natively.
     Shared window references must be constructed directly from the raw base
     through the consumer's published prefix, never over the writable append
     tail. `the_published_window_stays_disjoint_from_hash_append` holds the
     real window accessor across a synchronized `read_block` short read and
     EOF. The other `the_published_window_*` tests cover physical clamping,
     history, stored chunks, reuse and parked fallback state. These new tests
     are selected by the native and Miri kernel CI filters. Native validation
     of the bounded-prefix correction passes; Miri remains pending and is
     reserved for the final capstone. Native passes alone do not prove this
     reference invariant.
     The safe owned-input transfer into the single-threaded finder is covered
     by `owned_blocks_match_copied_windows_and_return_their_allocation` in
     `src/enc/lzma2_enc.rs`, natively and under Miri, including write errors
     and reuse through both owned and copied windows.
   - `shrink_result` owns a global-allocator byte vector and only reallocates
     to a nonzero size below its capacity; a null return preserves ownership.
     `shrinking_a_result_preserves_bytes_and_releases_capacity` checks empty,
     full and shortened buffers natively and in the Miri kernel job.
     `result_shrink_failure_preserves_solid_and_parallel_output` injects
     failure in both encoder paths natively.
   - The test-only allocator in `tests/encoder_alloc.rs` delegates successful
     allocations, reallocations and deallocations to `System`, and injects
     null returns for selected layouts. Its native tests are
     `hash_chain_head_allocation_failure_is_recoverable`,
     `decoder_dictionary_allocation_failure_is_recoverable`,
     `failed_reconfiguration_keeps_the_allocated_coder_usable`, and the
     result-shrink failure test above; this allocator is not part of the
     library or the Miri kernel job.
   - Low-hash/table access in `mix_matches_kind`:
     `the_mix_matches_kernel_matches_checked_tables`, native and Miri.
     The `mix_matches_sdk` position-relative address path, selected by the
     fixed x86-64 `asm` candidate, is also
     covered by `the_sdk_consumer_preserves_checked_streams_and_normalization`
     and `the_sdk_consumer_preserves_checked_held_block_tails`, comparing
     full LZ-owned state and outputs with the checked consumer across all
     finder families, both tail rules, and normalization. Native stream
     coverage explicitly proves window slides; Miri bounds the stream size.
   - `HeldRecords`, `consume_matches` and `consume_skip` in
     `src/enc/lz_find_mt.rs`: only real producer acquisition certifies the
     complete record partition. Raw pointers stay in one held block, and
     only numeric positions survive handoff. Output capacity is checked
     independently; short outputs and synthetic state retain the indexed
     fallback. Reset, allocation replacement and teardown revoke provenance.
     `held_records_preserve_short_output_and_synthetic_failure_state` checks
     full fallback state and output; the stream/normalization consumer tests,
     `skip_spans_preserve_checked_streams_and_normalization`, and
     `a_direct_block_is_read_as_the_copied_window_is` compare independent
     indexed consumption through interleavings, slides, cancellation and
     reuse. `the_sdk_fused_consumer_preserves_produced_threaded_tails`
     compares the fused positional BT4 consumer with the checked indexed
     consumer on actual producer records, including short tails, full output
     sentinels and consumer state. It passes natively; Miri remains pending.
     The register-walk tests also check successful record partitions
     for every native backend. Portable cursor Miri validation is required
     at the final capstone and remains pending; native tests do not establish
     Miri verification, and Miri never verifies the assembly producers.
   - High-hash table access in `get_heads` / `heads_loop`:
     `get_heads_matches_the_indexed_loop` compares heads and the entire
     mutated table against the indexed oracle for all seven families natively.
     Miri uses bounded 16-bit tables for trusted H2/H3/H4/H5. Big families
     retain full 24-bit trusted-domain native coverage; Miri exercises their
     checked fallback through `get_heads_domain_fallback_preserves_checked_mutations`,
     including arbitrary masks/CRC values, short tables/windows, and mutations
     before a panic. `get_heads_refuses_a_short_window` checks entry rejection.
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
   - SDK normalization kernels in `src/enc/lz_find/normalize.rs`:
     `normalization_matches_scalar_at_every_alignment_and_tail` checks prefix,
     bulk and tail against scalar saturation, including every word alignment.
     `normalization_x86_kernels_match_scalar` directly exercises SSE4.1 and
     AVX2 after runtime feature checks, natively only. Miri selects the scalar
     path and cannot verify the architecture kernels; ARM needs native tests.
   - Diagnostic BCJ2 pointer cursors in `src/filters/bcj2/cursor.rs`:
     `sdk_cursor_matches_checked_at_every_stream_and_output_edge` and
     `sdk_cursor_matches_checked_for_malformed_streams` compare output, input
     consumption, error and every decoder-state field, natively and in the
     Miri kernel job. The port is available only to tests and `kernel-ab`;
     default production decoding retains the checked slice implementation.
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
