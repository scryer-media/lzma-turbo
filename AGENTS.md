# Agent and contributor rules for lzma-turbo

These rules apply to every automated agent and every human contributor.

## What this repository is

An LZMA/LZMA2 library ported from Igor Pavlov's public-domain SDK: the decoder
from `LzmaDec.c` / `Lzma2Dec.c`, and the encoder from `LzFind.c`, `LzmaEnc.c`
and `Lzma2Enc.c`. The point of the crate is speed with parity to the
reference: byte-identical output from the decoder, and bit-exact output from
the encoder. Readability that costs throughput is not wanted.

Read before changing code:

- `docs/design-rules.md`: the dependency and performance rules.
- `docs/porting.md` and `docs/encoder.md`: where each part of the decoder
  and encoder came from, and the fast-loop invariants unsafe code relies on.
- `docs/unsafe-validation.md`: every unsafe surface and what checks it.
- `docs/security.md` and `SECURITY.md`: the limits and the threat model.
- `docs/benchmarking.md`: the acceptance gate.
- `docs/failed-experiments.md`: what has been tried and did not pay. Do not
  retry an entry without new evidence, and add to it when an experiment fails.

## Working on a task

- Carry a request through the edits, the checks that show it works, and the
  handoff. Settle routine, reversible choices from the code and docs rather
  than asking; ask when the answer changes correctness or what is authorised.
- Read the code you are changing, and its tests, before changing it. Extend
  what is there rather than adding a parallel path.
- Other agents and people work in the same checkouts. Inspect the working
  tree before editing, change only what the task needs, and treat someone
  else's uncommitted work or failing check as something to report, not fix.
- Keep plans, reports and handoffs out of the repository. Do not create,
  edit or delete Markdown files outside the task at hand.
- At handoff, say what changed, which checks ran and passed, which were not
  run, and what is still open. Do not claim a speed-up without a measurement.

## Correctness

- The reference is the oracle. Output is checked against the reference
  implementation, differentially, and a difference is a bug whatever the
  reason.
- No input panics. Malformed, truncated or hostile input returns an error; it
  never panics, reads or writes out of bounds, or does unbounded work.
- Every `unsafe` block carries a `// SAFETY:` comment naming the invariant it
  relies on and the check that established it. Code with `unsafe` is covered
  by Miri wherever Miri can run it; a test Miri cannot run is ignored under
  `cfg(miri)`, never the lane. Assembly and OS FFI are covered by native
  tests. `docs/unsafe-validation.md` is updated in the same change.
- A new public decoder or encoder entry point gets a fuzz target under
  `fuzz/` in the same change.

## Tests

A test gives the same result on a slow, loaded or shared runner as on an idle
workstation.

- Wait for the condition the assertion depends on, never for elapsed time.
  No fixed sleeps, no deadlines, no elapsed-time assertions, no timeouts as
  a stand-in for a condition.
- Do not assume one concurrent operation finishes before another unless the
  code guarantees that order.
- Never fix a flaky test with a longer timeout, a sleep or a retry. Find the
  race and remove it.
- Fixture bytes are generated, never committed.

## Performance

- A claimed speed-up shows the median of interleaved rounds against the
  previous release, on the hardware it targets, with outputs compared before
  timing. A change to one path shows the others unchanged.
- Hardware without the feature a change needs is part of the claim: an AVX2
  change is measured on an AVX-512 part too, and aarch64 whenever shared code
  moves.
- Raw benchmark data, machine-readable reports and profiling bundles stay
  outside the repository. Commit Markdown summaries and source identities;
  existing evidence is kept as `docs/benchmark-data.md` describes.

## Dependencies

- `Cargo.lock` is the source of truth: pass `--locked` to Cargo.
- A new library dependency is the maintainer's decision, not an agent's.
- Before upgrading dependencies, read `renovate.json` and honour every
  exception in it.

## Versions and pull requests

- Any code change to the crate (`src/`) bumps its version, `Cargo.lock` and
  `CHANGELOG.md` in the same change. The bump is what `cargo semver-checks`
  says it is. A documentation- or CI-only change says so instead.
- The version in a pull request is the latest one published on crates.io
  plus one increment, never more. An unpublished version heading is not a
  baseline: fold new work into it rather than bumping again.
- Releases go through the repository's release workflow
  (`docs/publishing.md`), never by hand.

## Repository hygiene

- Commits are SSH-signed. Never pass `--no-gpg-sign` or set
  `commit.gpgsign=false`; if signing fails, leave the change uncommitted.
- No `Co-Authored-By` or other attribution trailers in commit messages.
- Never run destructive git working-tree operations (`checkout --`,
  `restore`, `reset --hard`, `clean`, `stash`) in a shared checkout.
- Branch names use gitflow prefixes: `feature/…`, `bugfix/…`, `hotfix/…`.
  Worktrees go under `.worktrees/`.
- Agents do not push unless told to. The maintainer pushes.
- The pre-commit hook (`.githooks/pre-commit`) rejects home paths, the local
  username and secrets. Keep it enabled: `git config core.hooksPath .githooks`.
- Before a commit is proposed: `cargo fmt --all`, `cargo clippy --locked
  --workspace --all-targets -- -D warnings`, `cargo nextest run --locked
  --no-fail-fast` and `cargo test --locked --doc`. Install `cargo-nextest`
  0.9.143 as in CI, keep `cargo miri test` for interpreter checks, and
  exclude `wasm-conformance` from ordinary workspace sweeps; its own CI job
  runs it.

## Code review

Read `SECURITY.md` before reviewing, and judge findings against the threat
model it states.
