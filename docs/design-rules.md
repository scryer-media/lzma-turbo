# Design rules

The rules this crate's code holds to, beyond the general ones in
`AGENTS.md`. Read this before changing a decoder, encoder or filter, a
dependency, or anything measured by the acceptance gate.

## Porting

1. Port, do not redesign. Keep the reference decoder's structure: one decode
   loop, coder state in locals, per-symbol limit checks under a margin, a
   separate slow path near buffer edges. See `docs/porting.md`, and
   `docs/encoder.md` for the encoder's port rules and what was left out.
2. The reference is `C/` in a checkout of github.com/ip7z/7zip at the commit
   named in `docs/porting.md`. Cite the C function name in a comment when a
   Rust function corresponds to one.
3. `unsafe` is allowed where the C relies on the margin invariant, and
   elsewhere only with the verification recorded in
   `docs/unsafe-validation.md`, which lists every unsafe surface and the
   tests that check it.
4. Correctness is proven differentially: decoded bytes must equal what
   `xz -dc` / `7zz` produce for the same input, byte for byte, and malformed
   input must return an error rather than panic or read out of bounds.

## Dependencies

No dependencies in the decoder modules: `src/lzma/`, `src/lzma2/` and
everything they reach must build from `core` and `alloc` alone. The only
library dependencies the crate may take are `crc-turbo`, behind the `crc`
feature, and the SHA-256 backends — `aws-lc-rs` behind `crypto`, `sha2`
behind `native-crypto` — all optional, all confined to `src/crc.rs` and
`src/crypto/`, and none of them reachable from the decoder even when the
default features have them on. Anything else needs a decision from the
maintainer, not from an agent. Dev-dependencies and the bench tool may pull
what they need.

## Performance

- The acceptance gate is `docs/benchmarking.md`. A change that regresses the
  gate does not merge, however clean it is.
- Measure with `cargo run --release -p lzma-bench`, on an otherwise idle
  machine, at least three runs, and report the median.
- Fixture bytes are never committed. `cargo xtask fixtures` regenerates them.
