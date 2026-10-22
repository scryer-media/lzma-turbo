# Contributing

Thanks for looking. A few things before you open a pull request:

- Read [AGENTS.md](../AGENTS.md) and [docs/design-rules.md](../docs/design-rules.md);
  the porting and performance rules there apply to humans too.
- Run `cargo fmt --all`,
  `cargo clippy --workspace --exclude wasm-conformance --all-targets --all-features -- -D warnings`,
  `cargo nextest run --workspace --exclude wasm-conformance --no-fail-fast`
  and `cargo test --doc --workspace --exclude wasm-conformance --no-fail-fast` locally.
- If you touched the decoder or the encoder, include before/after numbers from
  `cargo run --release -p lzma-bench` per `docs/benchmarking.md`.
- Bump the crate version, `Cargo.lock` and the crate changelog in the
  same PR as the code change.
- Enable the hooks once per clone: `git config core.hooksPath .githooks`.

Install the native test runner with `cargo install cargo-nextest --locked --version 0.9.143`.
Nextest runs unit and integration tests; Cargo runs doctests, and Miri uses its own interpreter.
