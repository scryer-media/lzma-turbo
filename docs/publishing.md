# Publishing

Releases are published by `.github/workflows/release.yml`, started one of two
ways:

- **From GitHub Actions** (the usual way): run the `release` workflow on
  `main`. It releases the version in `Cargo.toml`; nothing is typed in. No
  local signing or tagging is involved; the workflow creates the tag. See
  "Each release" below.
- **From a machine**, with `cargo xtask release`, which pushes a signed tag
  that starts the workflow. The task (`xtask/src/main.rs`, plain Rust, so it
  runs wherever `cargo` does) runs every check the
  workflow runs, so a tag that reaches GitHub is one the workflow will accept.

## One-time setup, before the first release

1. **Publish the first version from a machine with a crates.io token.**
   crates.io only lets a workflow publish a crate that already exists, so the
   first release is cut with

   ```sh
   cargo xtask release --publish
   ```

   which runs the checks, creates the signed tag, runs `cargo publish` and
   then pushes the tag. The workflow sees the version on crates.io, skips its
   own upload and creates the GitHub release.
2. **Turn on trusted publishing** at
   `https://crates.io/crates/lzma-turbo/settings/new-trusted-publisher`:
   repository owner `scryer-media`, repository `lzma-turbo`, workflow
   `release.yml`, environment `crates-io`.
3. **Create the `crates-io` environment** in the GitHub repository settings.
   If it is restricted to refs, allow both the branch `main` (dispatched
   releases) and the tags `v*` (pushed tags). No secrets are needed, the
   workflow gets a short-lived token from crates.io through OIDC.

The release workflow needs `contents: write` to create the GitHub release,
which it requests for that job only.

## Each release

1. On `main`:
   - set `version` in `Cargo.toml` and in `tools/lzma-bench/Cargo.toml`, and
     refresh `Cargo.lock`;
   - give the version's `CHANGELOG.md` heading its date,
     `## 0.3.0 - 2026-09-17`;
   - update the dependency lines in `README.md` if the major.minor changed;
   - commit, signed, and push. Wait for `ci` to go green on that commit.
2. Actions → `release` → Run workflow, branch `main`, `dry_run` left on. The
   run releases the version in `Cargo.toml`. It refuses a branch other than
   `main`, a version below the newest on crates.io, a changelog section
   missing or still marked unreleased, a tag `v<version>` at another commit,
   a commit whose `ci-complete` is not green, a public API change larger
   than the version bump (`cargo semver-checks`), and a crate that does not
   package.
3. Run it again with `dry_run` off. It publishes to crates.io, then creates
   the tag `v<version>` at the commit it checked and the GitHub release with
   the changelog section as its notes. A run that fails after the upload can
   be rerun: a version already on crates.io, a tag already at that commit and
   a release that already exists are all taken as done.

`sevenz-turbo` is released separately once this version is on crates.io.

## From a machine, with a signed tag

1. Step 1 above.
2. `cargo xtask release --dry-run`, then `cargo xtask release`. The dry run
   works on any branch and on a dirty tree, and lists everything a real run
   would refuse rather than stopping at the first. A real run refuses a dirty
   tree, an unsigned HEAD, a branch other than `main`, a changelog section
   missing or still marked unreleased, a README that shows neither the version
   nor its major.minor, and a tag that exists anywhere but at HEAD (a signed tag already at HEAD is a
   release that stopped half way - a refused publish or push - and the run
   carries on from it). It then runs the tests
   in release mode (`--skip-tests` leaves them to CI and to the workflow's
   `verify` job) and a `cargo publish --dry-run`, creates the signed tag
   `v<version>` and pushes it.
3. The workflow verifies the tag against the manifest, tests, publishes to
   crates.io and creates the GitHub release with the changelog section as its
   notes. Nothing else needs doing; `sevenz-turbo` is released separately once
   this version is on crates.io.
