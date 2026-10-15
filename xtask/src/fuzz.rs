//! `cargo xtask fuzz <seconds> <workdir>`: the fuzz targets CI runs, seeded
//! from the committed vectors, with each target's corpus and any finding under
//! `<workdir>`.
//!
//! The budget is *split* between the targets rather than given to each: a
//! requested mutation time is shared however many targets this file grows.
//! Compilation, corpus startup and a final in-flight case are additional
//! wall time, bounded separately by the workflow and per-case timeout.
//!
//! Set `LZMA_SDK` (and on x86-64 Linux `LZMA_ORACLE_ASSEMBLER`) to put the
//! LZMA SDK's own decoders into the comparison; see tools/sdk-oracle/build.rs.
//! `FUZZ_TOOLCHAIN` picks the nightly (default `nightly`).
//!
//! `cargo xtask fuzz-check` builds every target and runs none, so a target
//! cannot rot between fuzzing runs.

use std::{
    env, fs,
    path::Path,
    process::{Command, ExitCode},
};

use crate::cmd::repo_root;

/// The `differential` target's second byte, picking the largest input and
/// output slices.
const WHOLE: u8 = 0x0F;

/// Every target a fuzzing run covers.
///
/// `differential` compares the decode loops with the C's and
/// `encode_differential` the encoders; `encode_round_trip` asks the question
/// the C cannot answer, which is that whatever this encoder produces, this
/// crate's own decoders return.
const TARGETS: &[&str] = &[
    "differential",
    "encode_round_trip",
    "encode_differential",
    "decode_lzma",
    "decode_lzma2",
    "decode_lzma2_mt",
    "decode_xz",
];

fn budgets(seconds: u64) -> Option<Vec<u64>> {
    let count = TARGETS.len() as u64;
    // A zero libFuzzer duration means unlimited, not zero work. Refuse a
    // budget too short to give every target a nonzero finite duration.
    (seconds >= count).then(|| {
        (0..count)
            .map(|i| seconds / count + u64::from(i < seconds % count))
            .collect()
    })
}

pub fn fuzz(mut args: impl Iterator<Item = String>) -> ExitCode {
    let (Some(seconds), Some(work)) = (args.next(), args.next()) else {
        eprintln!("usage: cargo xtask fuzz <seconds> <workdir>");
        return ExitCode::from(2);
    };
    let Ok(seconds) = seconds.parse::<u64>() else {
        eprintln!("xtask fuzz: <seconds> must be a number");
        return ExitCode::from(2);
    };
    let Some(budgets) = budgets(seconds) else {
        eprintln!(
            "xtask fuzz: need at least {} seconds for all targets",
            TARGETS.len()
        );
        return ExitCode::from(2);
    };
    let root = repo_root();
    let work = Path::new(&work);
    let artifacts = work.join("artifacts");
    if let Err(e) = fs::create_dir_all(&artifacts) {
        eprintln!("xtask fuzz: create {}: {e}", artifacts.display());
        return ExitCode::FAILURE;
    }

    let mut failures = Vec::new();
    for (target, each) in TARGETS.iter().zip(budgets) {
        let corpus = work.join(target);
        if let Err(e) = fs::create_dir_all(&corpus) {
            eprintln!("xtask fuzz: create {}: {e}", corpus.display());
            return ExitCode::FAILURE;
        }
        if let Err(e) = seed(&root, target, &corpus) {
            eprintln!("xtask fuzz: seeding {target}: {e}");
            return ExitCode::FAILURE;
        }
        println!("== {target}: {each}s");
        match run(&root, target, &corpus, &artifacts, each) {
            Ok(true) => {}
            Ok(false) => failures.push(*target),
            Err(e) => {
                eprintln!("xtask fuzz: {e}");
                failures.push(*target);
            }
        }
    }
    if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("xtask fuzz: failing targets: {}", failures.join(", "));
        ExitCode::FAILURE
    }
}

/// `cargo xtask fuzz-check`: every target compiled, none run.
///
/// A fuzz target is not built by `cargo test`, `cargo clippy` or `cargo doc` -
/// `fuzz/` is its own workspace - so an API change can leave one uncompilable
/// and nothing says so until the next fuzzing run.
pub fn fuzz_check(_args: impl Iterator<Item = String>) -> ExitCode {
    let root = repo_root();
    let toolchain = env::var("FUZZ_TOOLCHAIN").unwrap_or_else(|_| "nightly".into());
    let status = Command::new("cargo")
        .arg(format!("+{toolchain}"))
        // `--dev` is the unoptimized build: this proves the targets compile
        // and link, and nothing here is going to be run.
        .args(["fuzz", "build", "--dev"])
        .current_dir(root.join("fuzz"))
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("fuzz-check: every target builds");
            ExitCode::SUCCESS
        }
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("xtask fuzz-check: cannot run cargo fuzz: {e}");
            ExitCode::FAILURE
        }
    }
}

fn seed(root: &Path, target: &str, corpus: &Path) -> Result<(), String> {
    match target {
        "differential" => seed_differential(root, corpus),
        // The two encode targets read a settings prefix of their own length.
        "encode_round_trip" => seed_encode(root, corpus, 4),
        "encode_differential" => seed_encode(root, corpus, 6),
        "decode_lzma" => seed_container(root, corpus, "lzma", &[0, 3]),
        "decode_xz" => seed_container(root, corpus, "xz", &[0, 18]),
        "decode_lzma2" => seed_lzma2(root, corpus, false),
        "decode_lzma2_mt" => seed_lzma2(root, corpus, true),
        other => Err(format!("no seeds defined for {other}")),
    }
}

fn seed_container(
    root: &Path,
    corpus: &Path,
    extension: &str,
    shapes: &[u8],
) -> Result<(), String> {
    for path in entries(&root.join("tests/data"))? {
        if !path.extension().is_some_and(|e| e == extension) {
            continue;
        }
        let data = fs::read(&path).map_err(|e| e.to_string())?;
        for &shape in shapes {
            let mut seed = vec![shape];
            seed.extend_from_slice(&data);
            write_seed(corpus, &format!("shape-{shape}-{}", name_of(&path)), &seed)?;
        }
    }
    Ok(())
}

fn seed_lzma2(root: &Path, corpus: &Path, threaded: bool) -> Result<(), String> {
    for path in entries(&root.join("fuzz/corpus/decode_lzma2"))? {
        let data = fs::read(&path).map_err(|e| e.to_string())?;
        if data.len() < 2 {
            return Err(format!("malformed LZMA2 seed {}", name_of(&path)));
        }
        if threaded {
            // Unlike decode_lzma2's two-byte <shape,dict> prefix, the MT
            // target reads <threads/chase,dict,feed>. Preserve raw framing.
            for (mode, feed) in [(3, 3), (0x83, 1)] {
                let mut seed = vec![mode, data[1], feed];
                seed.extend_from_slice(&data[2..]);
                write_seed(corpus, &format!("mode-{mode}-{}", name_of(&path)), &seed)?;
            }
        } else {
            write_seed(corpus, &name_of(&path), &data)?;
        }
    }
    Ok(())
}

fn seed_differential(root: &Path, corpus: &Path) -> Result<(), String> {
    // The target's first byte picks LZMA (even) or LZMA2 (odd, with the
    // dictionary property above the low bit).
    let dir = root.join("tests/data");
    for path in entries(&dir)? {
        if path.extension().is_some_and(|e| e == "lzma") {
            let mut seed = vec![0, WHOLE];
            seed.extend(fs::read(&path).map_err(|e| e.to_string())?);
            write_seed(corpus, &name_of(&path), &seed)?;
        }
    }
    // decode_lzma2 seeds are <shape> <dictionary property> <stream>.
    let dir = root.join("fuzz/corpus/decode_lzma2");
    for path in entries(&dir)? {
        let bytes = fs::read(&path).map_err(|e| e.to_string())?;
        if bytes.len() < 2 {
            continue;
        }
        let mut seed = vec![((bytes[1] % 25) << 1) | 1, WHOLE];
        seed.extend_from_slice(&bytes[2..]);
        write_seed(corpus, &format!("lzma2-{}", name_of(&path)), &seed)?;
    }
    Ok(())
}

/// The sources `cargo xtask vectors` generates the decoder vectors *from*,
/// each behind the four-byte settings prefix the encode targets read.
///
/// An encoder wants plausible input, not a compressed stream: `src_*.bin` are
/// the plain bytes - text, random, zeros, a mixture - already in the
/// repository with a generator that explains every one.
fn seed_encode(root: &Path, corpus: &Path, prefix: usize) -> Result<(), String> {
    // Four prefixes, so each source arrives at more than one setting: a
    // hash-chain finder and a binary-tree one, a fast level and an optimal
    // one. The fourth byte is the encode targets' container and thread
    // selector. `encode_differential` reads its settings in a different order
    // and two bytes further, so the last two are for it and the others take
    // the first four; either way each seed covers more than one shape, which
    // is all a seed has to do.
    const PREFIXES: [(&str, [u8; 6]); 4] = [
        ("hc4-l1", [0, 4, 1, 0, 0, 0]),
        ("bt4-l9", [4, 6, 9, 0x5A, 3, 1]),
        ("bt2-l5", [2, 0, 5, 0xA5, 1, 2]),
        ("bt5-l0", [5, 7, 0, 0xFF, 4, 7]),
    ];
    let dir = root.join("tests/data");
    for path in entries(&dir)? {
        if !name_of(&path).starts_with("src_") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| e.to_string())?;
        for (tag, cfg) in PREFIXES {
            let mut seed = cfg[..prefix].to_vec();
            seed.extend_from_slice(&bytes);
            write_seed(corpus, &format!("{tag}-{}", name_of(&path)), &seed)?;
        }
    }
    Ok(())
}

fn entries(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        out.push(entry.map_err(|e| e.to_string())?.path());
    }
    out.sort();
    Ok(out)
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn write_seed(corpus: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    fs::write(corpus.join(name), bytes).map_err(|e| format!("write seed {name}: {e}"))
}

/// Runs one target; `Ok(false)` is a finding.
fn run(
    root: &Path,
    target: &str,
    corpus: &Path,
    artifacts: &Path,
    seconds: u64,
) -> Result<bool, String> {
    let toolchain = env::var("FUZZ_TOOLCHAIN").unwrap_or_else(|_| "nightly".into());
    // One directory for every target's findings, so the workflow's
    // upload-on-failure step needs no per-target path.
    let mut artifact_prefix = artifacts.display().to_string();
    artifact_prefix.push(std::path::MAIN_SEPARATOR);
    let status = Command::new("cargo")
        .arg(format!("+{toolchain}"))
        .args(["fuzz", "run", target])
        .arg(corpus)
        .arg("--")
        .arg(format!("-max_total_time={seconds}"))
        .arg("-max_len=65536")
        .arg("-timeout=30")
        .arg("-rss_limit_mb=1024")
        .arg(format!("-artifact_prefix={artifact_prefix}"))
        .current_dir(root.join("fuzz"))
        .status()
        .map_err(|e| format!("cannot run cargo fuzz: {e}"))?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_fuzz_binary_gets_a_finite_budget() {
        let manifest = include_str!("../../fuzz/Cargo.toml");
        let mut registered: Vec<_> = manifest
            .split("[[bin]]")
            .skip(1)
            .map(|section| {
                section
                    .lines()
                    .find_map(|line| {
                        line.trim()
                            .strip_prefix("name = ")
                            .map(|name| name.trim_matches('"'))
                    })
                    .expect("every fuzz binary has a name")
            })
            .collect();
        registered.sort_unstable();
        let mut scheduled = TARGETS.to_vec();
        scheduled.sort_unstable();
        assert_eq!(
            registered, scheduled,
            "a fuzz binary was omitted or scheduled twice"
        );
        for seconds in 0..=240 {
            match budgets(seconds) {
                None => assert!(seconds < registered.len() as u64),
                Some(slots) => {
                    assert_eq!(slots.len(), registered.len());
                    assert!(slots.iter().all(|&seconds| seconds > 0));
                    assert_eq!(slots.iter().sum::<u64>(), seconds);
                }
            }
        }
    }

    #[test]
    fn every_scheduled_target_has_seeds_with_the_right_framing() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("fuzz-seed-contract-{}-{nonce}", std::process::id());
        let scratch = Scratch(env::temp_dir().join(name));
        fs::create_dir(&scratch.0).unwrap();
        let root = repo_root();
        for target in TARGETS {
            let corpus = scratch.0.join(target);
            fs::create_dir(&corpus).unwrap();
            seed(&root, target, &corpus).unwrap();
            assert!(
                !entries(&corpus).unwrap().is_empty(),
                "{target} has no seeds"
            );
        }
        let original = fs::read(root.join("fuzz/corpus/decode_lzma2/mixed.lzma2.seed")).unwrap();
        for mode in [3, 0x83] {
            let changed = fs::read(
                scratch
                    .0
                    .join("decode_lzma2_mt")
                    .join(format!("mode-{mode}-mixed.lzma2.seed")),
            )
            .unwrap();
            assert_eq!(changed[0], mode);
            assert_eq!(changed[1], original[1]);
            assert_eq!(changed[2], if mode == 3 { 3 } else { 1 });
            assert_eq!(&changed[3..], &original[2..]);
        }
        for (target, extension, shape) in [("decode_lzma", "lzma", 3), ("decode_xz", "xz", 18)] {
            let name = format!("tiny.p1.{extension}");
            let original = fs::read(root.join("tests/data").join(&name)).unwrap();
            let seed =
                fs::read(scratch.0.join(target).join(format!("shape-{shape}-{name}"))).unwrap();
            assert_eq!(seed[0], shape);
            assert_eq!(&seed[1..], &original);
        }
    }
}
