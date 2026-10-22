# Design rules

The rules this crate's code holds to, beyond the general ones in
`AGENTS.md`. Read this before changing a dependency, or anything measured
by the acceptance gate.

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
