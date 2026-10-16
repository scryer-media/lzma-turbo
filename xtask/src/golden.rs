//! `cargo xtask golden [--check|--write]`: the cross-platform byte identity
//! vectors, the way `cargo xtask vectors` handles the decoder's.
//!
//! The cases themselves are in `tests/golden.rs`, not here, because producing
//! them means *running this crate's encoder* and this binary deliberately
//! depends on nothing. So both directions are that one test: `--check` runs
//! it, and `--write` runs it with the variable that makes it write the
//! manifest instead of comparing against it.

use std::process::{Command, ExitCode};

use crate::cmd::repo_root;

pub fn golden(mut args: impl Iterator<Item = String>) -> ExitCode {
    let write = match args.next().as_deref() {
        None | Some("--check") => false,
        Some("--write") => true,
        Some(other) => {
            eprintln!("usage: cargo xtask golden [--check|--write]  (not {other})");
            return ExitCode::from(2);
        }
    };
    let mut cmd = Command::new("cargo");
    cmd.args([
        "nextest",
        "run",
        "--no-fail-fast",
        "--locked",
        "--release",
        "-p",
        "lzma-turbo",
        "--test",
        "golden",
    ])
    .current_dir(repo_root());
    if write {
        cmd.env("LZMA_TURBO_GOLDEN_WRITE", "1");
    }
    // The digests are of the whole corpus; a cap would compare other streams.
    cmd.env_remove("LZMA_TURBO_CORPUS_MAX");
    match cmd.status() {
        Ok(s) if s.success() => {
            println!(
                "golden: {}",
                if write {
                    "tests/golden.manifest written"
                } else {
                    "the committed digests are what this build produces"
                }
            );
            ExitCode::SUCCESS
        }
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("xtask golden: cannot run cargo nextest run: {e}");
            ExitCode::FAILURE
        }
    }
}
