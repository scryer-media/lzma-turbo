---
name: open-pr
description: Open, push to, or report on a pull request in lzma-turbo. Runs the preflight a mergeable PR needs (one PR per version, the version bump, a dated changelog heading, README dependency lines, lockfile, fmt, clippy, tests, semver-checks, package) and writes the PR body. Use whenever a PR is to be opened, pushed, updated or called ready.
---

# Opening a pull request in lzma-turbo

Write every PR as if it were already released. The moment it merges, `main`
must release by dispatching the `release` workflow with nothing else to edit:
the workflow releases the version in `Cargo.toml` and refuses anything below.
Each step here exists because skipping it has already failed a release.

## 1. One PR per version

- `gh pr list --repo scryer-media/lzma-turbo --state open`. If a PR for the
  next version is open, this work goes onto its branch as more commits. Never
  open a second PR for the same version, and never open a PR whose base is
  another PR's branch.
- Branches use `feature/…`, `bugfix/…` or `hotfix/…`, cut from `main`, in a
  worktree under `.worktrees/`.
- Open the PR only when the maintainer asked for one. Agents do not push
  otherwise.

## 2. The version

- Published is what crates.io says, nothing else: not a changelog heading, not
  a tag, not a review bot.
  `curl -s https://crates.io/api/v1/crates/lzma-turbo | jq -r .crate.max_version`
- The PR's version is published plus one step. If `main` already carries an
  unpublished version above published, use it; never bump twice before a
  release.
- Which step: run `cargo semver-checks -p lzma-turbo` against the published
  baseline. A breaking change on 0.x is a minor bump (0.9.2 → 0.10.0);
  anything else is a patch bump.
- Any change under `src/` needs the bump. Set `version` in `Cargo.toml` and in
  `tools/lzma-bench/Cargo.toml`, then `cargo update -p lzma-turbo --offline`
  and confirm `cargo metadata --locked --format-version 1 >/dev/null` passes.

## 3. Changelog

- The section heading is `## <version> - <YYYY-MM-DD>`, dated the day the PR
  is opened. Never `Unreleased`: the release workflow refuses a heading that
  says it, and the 0.10.0 release run failed on exactly that.
- `grep -n -i unreleased CHANGELOG.md` must print nothing.
- Every change in the PR has a bullet under that one heading. A later commit on
  the same PR adds bullets; it does not add a heading.

## 4. README

- Dependency lines name the new major.minor:
  `grep -n 'lzma-turbo = \|version = "' README.md`. A minor bump changes every
  `lzma-turbo = "0.x"` and `{ version = "0.x", … }` line.
- A new or renamed feature, default or public type is reflected in the README
  and in `docs/` in the same PR.

## 5. Local gates

Run what CI runs, with `--locked`, before pushing:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --exclude wasm-conformance --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --all-features
cargo nextest run --locked --workspace --exclude wasm-conformance --no-fail-fast
cargo test --doc --locked --workspace --exclude wasm-conformance --no-fail-fast
cargo xtask golden --check
cargo xtask vectors --check
cargo semver-checks -p lzma-turbo
cargo package --locked -p lzma-turbo
```

- A change behind a feature also runs that feature's lane from the `test` job
  in `.github/workflows/ci.yml` (`--no-default-features`, `std`, `enc`,
  `enc,xz`, `native-crypto`).
- A change to `unsafe` code, the assembly kernels or the match finder runs the
  selectors listed in `AGENTS.md` for it.
- Keep the pre-commit hook on: `git config core.hooksPath .githooks`.

## 6. Commit

- Every commit is SSH-signed. If signing fails, stop and leave the change
  staged; never `--no-gpg-sign`.
- No `Co-Authored-By` or other attribution trailers.
- Conventional subject: `feat(scope): …`, `fix(scope): …`, `!` for breaking.

## 7. PR title and body

Title: the conventional subject plus the version, for example
`feat(error)!: type every failure (0.10.0)`.

Body sections, in this order, leaving out any that would be empty:

- **What**: one paragraph.
- **Why**: the problem it fixes or the consumer that needs it.
- **Changes**: grouped by area.
- **Breaking changes**: what a consumer must change, if anything.
- **Merge order**: anything that has to publish or merge first.
- **Gates**: each command from step 5 and its result.
- **Follow-ups**: work left for later. Follow-ups live in the body; do not
  open GitHub issues without the maintainer's approval.

## 8. After every push

- Do not enable auto-merge unless the maintainer asks.
- Read the review comments:
  `gh api repos/scryer-media/lzma-turbo/pulls/<n>/comments --paginate`.
  Fix each finding on this branch, or reply with the reason it is wrong, then
  resolve the thread.
- A code-scanning alert on the PR is fixed, or dismissed with a written reason
  when it is a false positive.
- The PR is ready only when every job, `ci-complete` included, is green on the
  head and every correctness, safety or version finding is fixed and replied
  to. Pure wording nits may be deferred with a reply.

## 9. After merge

`ci-complete` must be green on the merge commit on `main`. Then Actions →
`release` → Run workflow on `main`, `dry_run` on, then again with it off. See
`docs/publishing.md`.
