//! Records which commit `lzma-bench` was built from, for `--shot info`.
//!
//! The harness measures a binary it may not have built itself, and labels the
//! report with the checkout's commit; it compares the two and refuses a
//! binary it cannot tie to that commit. So the binary carries the commit and
//! whether the sources it was built from had uncommitted changes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the binary is built from, relative to the repository root.
const SOURCES: [&str; 5] = [
    "src",
    "Cargo.toml",
    "Cargo.lock",
    ".cargo",
    "tools/lzma-bench",
];

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let repo = manifest.join("..").join("..");

    // Rerun when a source changes, so the dirty flag follows the tree, and
    // when HEAD or the branch it names moves, so the commit does.
    for source in SOURCES {
        println!("cargo:rerun-if-changed={}", repo.join(source).display());
    }
    let resolve = |p: Option<String>| p.map(|p| repo.join(p));
    if let Some(head) = resolve(git(&repo, &["rev-parse", "--git-path", "HEAD"])) {
        println!("cargo:rerun-if-changed={}", head.display());
    }
    if let Some(name) = git(&repo, &["symbolic-ref", "-q", "HEAD"])
        && let Some(r) = resolve(git(&repo, &["rev-parse", "--git-path", &name]))
    {
        println!("cargo:rerun-if-changed={}", r.display());
    }
    if let Some(packed) = resolve(git(&repo, &["rev-parse", "--git-path", "packed-refs"]))
        && packed.exists()
    {
        println!("cargo:rerun-if-changed={}", packed.display());
    }

    let commit = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_default();
    let mut status = vec!["status", "--porcelain", "--untracked-files=no", "--"];
    status.extend(SOURCES);
    let dirty = match (commit.is_empty(), git(&repo, &status)) {
        (false, Some(changes)) => {
            if changes.is_empty() {
                "false"
            } else {
                "true"
            }
        }
        _ => "",
    };
    println!("cargo:rustc-env=LZMA_BENCH_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=LZMA_BENCH_BUILD_DIRTY={dirty}");
}
