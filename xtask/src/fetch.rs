//! The upstream inputs the trust jobs compare this crate against, fetched
//! pinned: `cargo xtask sdk`, `cargo xtask jwasm` and `cargo xtask xz-tests`.
//!
//! Every one is named by something that cannot move under it. A git
//! dependency is fetched by commit, not by tag or by GitHub's generated
//! archive: a commit id names exactly one tree, a tag can be re-pointed, and
//! an archive's bytes are not promised to stay the same. A release tarball is
//! checked against its SHA-256 before anything is taken out of it.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use crate::cmd::repo_root;

/// The LZMA SDK source the decode loops were ported from (tag 26.03).
/// tools/asm-provenance and tools/sdk-oracle pin the SHA-256 of every file
/// they take from it.
const SDK_REPO: &str = "https://github.com/ip7z/7zip.git";
const SDK_COMMIT: &str = "0766b733fe3e06dd2a7f9a3cfbf2108ac73abd17";

/// JWasm, the MASM-compatible assembler the SDK's own Linux build uses for
/// `Asm/x86` (`MY_ASM = jwasm` in `CPP/7zip/7zip_gcc.mak`); tag v2.20, the
/// latest non-prerelease.
const JWASM_REPO: &str = "https://github.com/Baron-von-Riedesel/JWasm.git";
const JWASM_COMMIT: &str = "ac54827ff40b77ecd6e77ed0866a43fc60cd5fe1";

/// XZ Utils, for its decoder test files. The same release, and so the same
/// digest, that `.github/scripts/install-xz.sh` builds `xz` from.
const XZ_VERSION: &str = "5.8.3";
const XZ_SHA256: &str = "3d3a1b973af218114f4f889bbaa2f4c037deaae0c8e815eec381c3d546b974a0";

/// The test files of XZ Utils 5.6.0 and 5.6.1 that carried the CVE-2024-3094
/// payload. No clean release has them; if one ever appears in a tarball this
/// task is pointed at, something is very wrong.
const BACKDOOR_FILES: &[&str] = &["bad-3-corrupt_lzma2.xz", "good-large_compressed.lzma"];

/// 7-Zip's own console binary, the second external reader `tests/xz_encoder.rs`
/// puts this crate's `.xz` output through. 26.03 is the release whose `C/`
/// tree is the pinned SDK commit the whole port is made from, so the checker
/// and the reference encoder are the same code.
///
/// Every asset is fetched from the release by name and checked against its
/// SHA-256 below, the way [`download`] checks the XZ Utils tarball. Unix gets
/// the `7zz` the project ships; Windows has no `7zz`, so it gets `7za.exe`
/// out of the "extra" package, extracted with `7zr.exe` - which is a plain
/// executable and needs nothing to unpack it, which is the whole reason that
/// is the shape of this.
const SEVENZIP_VERSION: &str = "26.03";
const SEVENZIP_ASSETS: &[(&str, &str, &str)] = &[
    (
        "linux-x86_64",
        "7z2603-linux-x64.tar.xz",
        "dc99eff5008f1ab79bd7084c68513701547a808a89502bf4133683535ab3c695",
    ),
    (
        "linux-aarch64",
        "7z2603-linux-arm64.tar.xz",
        "2389ba20e4d8295e8709c20b6263b69bd1ec4972fe38a04ad7a1badbf595b996",
    ),
    (
        "macos",
        "7z2603-mac.tar.xz",
        "5ca87677072c59f5602e5c49baa27d4694bacd2259b4e507f0094249d4281480",
    ),
    (
        "windows-unpacker",
        "7zr.exe",
        "ad4c82fadcbdf93c03b4fc440f300509c7d60c5c2f4d183e35d9d70d6957037d",
    ),
    (
        "windows",
        "7z2603-extra.7z",
        "191894e6acb3647ffb69ce630479ff318523b2e2b9890aa7f05c1127c2e59b8f",
    ),
];

pub fn sdk(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(dest) = args.next() else {
        eprintln!("usage: cargo xtask sdk <destination>");
        return ExitCode::from(2);
    };
    result(
        git_at_commit(SDK_REPO, SDK_COMMIT, Path::new(&dest)).map(|()| {
            println!("LZMA SDK at {SDK_COMMIT} in {dest}");
        }),
    )
}

/// Builds JWasm under the given directory and prints the binary's path, for
/// CI to put in `LZMA_ORACLE_ASSEMBLER`. Unix only: it builds with make.
pub fn jwasm(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(dest) = args.next() else {
        eprintln!("usage: cargo xtask jwasm <build directory>");
        return ExitCode::from(2);
    };
    let dest = Path::new(&dest);
    result((|| {
        git_at_commit(JWASM_REPO, JWASM_COMMIT, dest)?;
        let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
        let status = Command::new("make")
            .args(["--silent", "-f", "GccUnix.mak", &format!("-j{jobs}")])
            .current_dir(dest)
            .stdout(std::io::stderr())
            .status()
            .map_err(|e| format!("run make: {e}"))?;
        if !status.success() {
            return Err("building JWasm failed".into());
        }
        let binary = fs::canonicalize(dest.join("build/GccUnixR/jwasm"))
            .map_err(|e| format!("no JWasm binary after the build: {e}"))?;
        println!("{}", binary.display());
        Ok(())
    })())
}

/// Copies XZ Utils' `.xz` and `.lzma` test files into the given directory
/// (default `target/xz-utils-tests`), each checked against
/// `tests/xz-utils.manifest`.
pub fn xz_tests(mut args: impl Iterator<Item = String>) -> ExitCode {
    let root = repo_root();
    let dest = args
        .next()
        .map_or_else(|| root.join("target").join("xz-utils-tests"), PathBuf::from);
    result(xz_tests_into(&root, &dest))
}

fn xz_tests_into(root: &Path, dest: &Path) -> Result<(), String> {
    let manifest = manifest(root)?;
    let work = root.join("target").join("xz-utils-fetch");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    let name = format!("xz-{XZ_VERSION}.tar.gz");
    let tarball = work.join(&name);
    let url =
        format!("https://github.com/tukaani-project/xz/releases/download/v{XZ_VERSION}/{name}");
    download(&url, &tarball, XZ_SHA256)?;

    let inner = format!("xz-{XZ_VERSION}/tests/files");
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&name)
        .arg(&inner)
        .current_dir(&work)
        .status()
        .map_err(|e| format!("run tar: {e}"))?;
    if !status.success() {
        return Err(format!("tar could not extract {inner}"));
    }

    let extracted = work.join(&inner);
    let mut found = Vec::new();
    for entry in fs::read_dir(&extracted).map_err(|e| e.to_string())? {
        let file = entry.map_err(|e| e.to_string())?.file_name();
        let file = file.to_string_lossy().into_owned();
        if BACKDOOR_FILES.contains(&file.as_str()) {
            return Err(format!(
                "{file} is in the tarball: it is one of the files that carried the \
                 CVE-2024-3094 payload, and no release this task should fetch has it"
            ));
        }
        if file.ends_with(".xz") || file.ends_with(".lzma") {
            found.push(file);
        }
    }
    found.sort();
    let listed: Vec<&String> = manifest.keys().collect();
    if found.iter().collect::<Vec<_>>() != listed {
        let extra: Vec<_> = found
            .iter()
            .filter(|f| !manifest.contains_key(*f))
            .collect();
        let missing: Vec<_> = listed.iter().filter(|f| !found.contains(f)).collect();
        return Err(format!(
            "the tarball's test files are not the manifest's: not listed {extra:?}, absent {missing:?}"
        ));
    }

    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for (file, digest) in &manifest {
        let bytes = fs::read(extracted.join(file)).map_err(|e| format!("read {file}: {e}"))?;
        let actual = sha256_hex(&bytes);
        if &actual != digest {
            return Err(format!(
                "{file}: SHA-256 {actual}, the manifest says {digest}"
            ));
        }
        fs::write(dest.join(file), bytes).map_err(|e| format!("write {file}: {e}"))?;
    }
    let _ = fs::remove_dir_all(&work);
    println!(
        "xz-tests: {} files from XZ Utils {XZ_VERSION} in {}",
        manifest.len(),
        dest.display()
    );
    Ok(())
}

/// Installs 7-Zip's console binary under the given directory (default
/// `target/sevenzip`) and prints its path, for CI to put in
/// `LZMA_TURBO_7ZZ`.
pub fn sevenzip(mut args: impl Iterator<Item = String>) -> ExitCode {
    let root = repo_root();
    let dest = args
        .next()
        .map_or_else(|| root.join("target").join("sevenzip"), PathBuf::from);
    result(sevenzip_into(&dest).map(|binary| println!("{}", binary.display())))
}

fn sevenzip_asset(key: &str) -> Result<(&'static str, &'static str), String> {
    SEVENZIP_ASSETS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, name, digest)| (*name, *digest))
        .ok_or_else(|| format!("no pinned 7-Zip asset for {key}"))
}

fn sevenzip_url(name: &str) -> String {
    format!("https://github.com/ip7z/7zip/releases/download/{SEVENZIP_VERSION}/{name}")
}

fn sevenzip_into(dest: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let key = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "linux-x86_64"
    } else {
        return Err("no pinned 7-Zip build for this platform".into());
    };
    let (name, digest) = sevenzip_asset(key)?;
    let archive = dest.join(name);
    download(&sevenzip_url(name), &archive, digest)?;

    let binary = if cfg!(windows) {
        // `7zr.exe` is a whole program in one file, so unpacking the "extra"
        // package needs nothing that is not pinned here.
        let (unpacker_name, unpacker_digest) = sevenzip_asset("windows-unpacker")?;
        let unpacker = dest.join(unpacker_name);
        download(&sevenzip_url(unpacker_name), &unpacker, unpacker_digest)?;
        let status = Command::new(&unpacker)
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", dest.display()))
            .arg(&archive)
            .arg("x64/7za.exe")
            .arg("x64/7za.dll")
            .status()
            .map_err(|e| format!("run {}: {e}", unpacker.display()))?;
        if !status.success() {
            return Err(format!("{unpacker_name} could not unpack {name}"));
        }
        dest.join("x64").join("7za.exe")
    } else {
        // `tar` reads `.tar.xz` on every runner this is meant for: bsdtar on
        // macOS decompresses it itself, GNU tar on Linux hands it to the `xz`
        // that `install-xz.sh` has already put on PATH.
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("7zz")
            .current_dir(dest)
            .status()
            .map_err(|e| format!("run tar: {e}"))?;
        if !status.success() {
            return Err(format!("tar could not take 7zz out of {name}"));
        }
        dest.join("7zz")
    };
    if !binary.is_file() {
        return Err(format!("{} is not there after unpacking", binary.display()));
    }
    // The tar carries its own mode, but the Windows path and a umask that
    // strips execute would both leave it unrunnable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = fs::metadata(&binary)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(perms.mode() | 0o755);
        fs::set_permissions(&binary, perms).map_err(|e| e.to_string())?;
    }
    let out = Command::new(&binary)
        .arg("i")
        .output()
        .map_err(|e| format!("run {}: {e}", binary.display()))?;
    if !out.status.success() {
        return Err(format!("{} does not run", binary.display()));
    }
    Ok(binary)
}

/// File name to SHA-256, from `tests/xz-utils.manifest`.
fn manifest(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let path = root.join("tests").join("xz-utils.manifest");
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [digest, _, _, file] = fields[..] else {
            return Err(format!("manifest line is not four fields: {line}"));
        };
        out.insert(file.to_owned(), digest.to_owned());
    }
    Ok(out)
}

/// Fetches `commit` of `repo` into a fresh repository at `dest`, and proves
/// that is what was checked out.
fn git_at_commit(repo: &str, commit: &str, dest: &Path) -> Result<(), String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dest)
            .args(args)
            .output()
            .map_err(|e| format!("run git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    git(&["init", "--quiet"])?;
    // The files are checked against pinned SHA-256s, so they must be the
    // committed bytes. Windows runners set core.autocrlf, which would rewrite
    // every LF on checkout; `* -text` in info/attributes outranks both that
    // and the fetched tree's own .gitattributes.
    git(&["config", "core.autocrlf", "false"])?;
    fs::write(dest.join(".git/info/attributes"), "* -text\n").map_err(|e| e.to_string())?;
    git(&["fetch", "--quiet", "--depth", "1", repo, commit])?;
    git(&[
        "-c",
        "advice.detachedHead=false",
        "checkout",
        "--quiet",
        "FETCH_HEAD",
    ])?;
    let head = git(&["rev-parse", "HEAD"])?;
    if head != commit {
        return Err(format!("{repo}: checked out {head}, wanted {commit}"));
    }
    Ok(())
}

/// Downloads `url` to `to` with curl, which ships with every OS CI runs on,
/// and fails unless the bytes have the given SHA-256.
fn download(url: &str, to: &Path, digest: &str) -> Result<(), String> {
    let status = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--retry",
            "3",
        ])
        .arg("--output")
        .arg(to)
        .arg(url)
        .status()
        .map_err(|e| format!("run curl: {e}"))?;
    if !status.success() {
        return Err(format!("download of {url} failed"));
    }
    let bytes = fs::read(to).map_err(|e| e.to_string())?;
    let actual = sha256_hex(&bytes);
    if actual != digest {
        return Err(format!("{url}: SHA-256 {actual}, pinned {digest}"));
    }
    Ok(())
}

fn result(r: Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The reference LZMA encoder, for the bit-exactness tests.
///
/// Builds the reference binaries from the pinned SDK into `target/lzma-util/`:
///
/// * `lzma`, `C/Util/Lzma/LzmaUtil.c` exactly as it ships, which encodes with
///   `LzmaEncProps_Init`'s defaults;
/// * `lzma-oracle`, the small harness below, which takes every setting
///   `tests/lzma_parity.rs` varies. `LzmaUtil` cannot: it never calls
///   `LzmaEnc_SetProps` with anything but the defaults, so on its own it
///   would pin one level and one match finder;
/// * `lzma2-oracle`, the same for `C/Lzma2Enc.c`, pinned to one solid block
///   and one thread;
/// * `lzma-oracle-mt` and `lzma2-oracle-mt`, those two again built *without*
///   `Z7_ST`, which is the only way to reach `LzFindMt.c` and `MtCoder.c`;
/// * `filter-oracle`, the SDK's branch converters and delta filter;
/// * `bcj2-oracle`, the SDK's four-stream BCJ2 converter, both directions.
///
/// The single-threaded ones are built with `-DZ7_ST`, which is what selects
/// `LzFind.c` over `LzFindMt.c`.
pub fn lzma_util(mut args: impl Iterator<Item = String>) -> ExitCode {
    let dest = args
        .next()
        .map_or_else(|| repo_root().join("target/lzma-util"), PathBuf::from);
    result(build_lzma_util(&dest).map(|built| {
        for path in built {
            println!("{}", path.display());
        }
    }))
}

fn build_lzma_util(dest: &Path) -> Result<Vec<PathBuf>, String> {
    let sdk = dest.join("sdk");
    if !sdk.join("C/LzmaEnc.c").is_file() {
        git_at_commit(SDK_REPO, SDK_COMMIT, &sdk)?;
    }
    let c = sdk.join("C");

    let write_src = |name: &str, body: &str| -> Result<PathBuf, String> {
        let path = dest.join(name);
        fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path)
    };
    let oracle_src = write_src("lzma-oracle.c", ORACLE_C)?;
    let oracle_mt_src = write_src("lzma-oracle-mt.c", ORACLE_MT_C)?;
    let oracle2_src = write_src("lzma2-oracle.c", ORACLE2_C)?;
    let oracle2mt_src = write_src("lzma2-oracle-mt.c", ORACLE2_MT_C)?;
    let filter_src = write_src("filter-oracle.c", ORACLE_FILTER_C)?;
    let bcj2_src = write_src("bcj2-oracle.c", ORACLE_BCJ2_C)?;

    // C: the `Z7_ST` build. `-D_7ZIP_ST` is the older spelling the SDK still
    // honours; passing both keeps this working either way.
    let common: &[&str] = &["-O2", "-DZ7_ST", "-D_7ZIP_ST"];
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());

    let build_with = |out: &Path, sources: &[PathBuf], flags: &[&str]| -> Result<(), String> {
        let mut cmd = Command::new(&cc);
        cmd.args(flags).arg("-I").arg(&c).arg("-o").arg(out);
        cmd.args(sources);
        let st = cmd
            .status()
            .map_err(|e| format!("run {cc}: {e}; set CC to a C compiler"))?;
        if !st.success() {
            return Err(format!("{cc} failed building {}", out.display()));
        }
        Ok(())
    };
    let build = |out: &Path, sources: &[PathBuf]| -> Result<(), String> {
        build_with(out, sources, common)
    };

    let core = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(|n| c.join(n)).collect() };
    // MinGW's gcc appends `.exe` to an output name that has no extension, so
    // the name the tests look for has to carry it too; on every other host
    // the suffix is empty.
    let binary =
        |dest: &Path, name: &str| dest.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));

    let util = binary(dest, "lzma");
    let mut util_srcs = core(&[
        "Util/Lzma/LzmaUtil.c",
        "Alloc.c",
        "CpuArch.c",
        "LzFind.c",
        "LzmaDec.c",
        "LzmaEnc.c",
        "7zFile.c",
        "7zStream.c",
    ]);
    util_srcs.sort();
    build(&util, &util_srcs)?;

    let oracle = binary(dest, "lzma-oracle");
    let mut oracle_srcs = vec![oracle_src];
    oracle_srcs.extend(core(&["Alloc.c", "CpuArch.c", "LzFind.c", "LzmaEnc.c"]));
    build(&oracle, &oracle_srcs)?;

    // The LZMA1 oracle again, built *without* `Z7_ST`, which is the only way
    // to reach `LzFindMt.c`. It takes `numThreads`, so the threaded match
    // finder can be compared against the C that actually threads.
    let oracle_mt = binary(dest, "lzma-oracle-mt");
    let mut oracle_mt_srcs = vec![oracle_mt_src];
    oracle_mt_srcs.extend(core(&[
        "Alloc.c",
        "CpuArch.c",
        "LzFind.c",
        "LzFindMt.c",
        "LzFindOpt.c",
        "LzmaEnc.c",
        "Threads.c",
    ]));
    let mut mt_flags: Vec<&str> = vec!["-O2"];
    if !cfg!(windows) {
        mt_flags.push("-pthread");
    }
    build_with(&oracle_mt, &oracle_mt_srcs, &mt_flags)?;

    let oracle2 = binary(dest, "lzma2-oracle");
    let mut oracle2_srcs = vec![oracle2_src];
    oracle2_srcs.extend(core(&[
        "Alloc.c",
        "CpuArch.c",
        "LzFind.c",
        "LzmaEnc.c",
        "Lzma2Enc.c",
    ]));
    build(&oracle2, &oracle2_srcs)?;

    // The same LZMA2 oracle built *without* `Z7_ST`, which is the only way to
    // reach `MtCoder.c` and `LzFindMt.c`. It takes a block size, a block
    // thread count and a match-finder thread count, so the threaded ports can
    // be compared against the C that actually threads.
    let oracle2_mt = binary(dest, "lzma2-oracle-mt");
    let mut oracle2_mt_srcs = vec![oracle2mt_src];
    oracle2_mt_srcs.extend(core(&[
        "Alloc.c",
        "CpuArch.c",
        "LzFind.c",
        "LzFindMt.c",
        "LzFindOpt.c",
        "LzmaEnc.c",
        "Lzma2Enc.c",
        "MtCoder.c",
        "MtDec.c",
        "Threads.c",
        "7zStream.c",
    ]));
    build_with(&oracle2_mt, &oracle2_mt_srcs, &mt_flags)?;

    let filters = binary(dest, "filter-oracle");
    let mut filter_srcs = vec![filter_src];
    filter_srcs.extend(core(&[
        "CpuArch.c",
        "Bra.c",
        "Bra86.c",
        "BraIA64.c",
        "Delta.c",
    ]));
    build(&filters, &filter_srcs)?;

    let bcj2 = binary(dest, "bcj2-oracle");
    let mut bcj2_srcs = vec![bcj2_src];
    bcj2_srcs.extend(core(&["CpuArch.c", "Bcj2.c", "Bcj2Enc.c"]));
    build(&bcj2, &bcj2_srcs)?;

    Ok(vec![
        util, oracle, oracle_mt, oracle2, oracle2_mt, filters, bcj2,
    ])
}

/// The SDK's own branch converters and delta filter, driven from the command
/// line, so the filter port can be compared byte for byte against them.
const ORACLE_FILTER_C: &str = r##"/* The SDK's BCJ and delta filters, for parity testing.
   Usage: filter-oracle <name> <enc|dec> <start-offset-or-distance> <in> <out>
   <name> is one of x86 ppc ia64 arm armt sparc arm64 riscv delta. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "Bra.h"
#include "Delta.h"

int main(int argc, char **argv)
{
  if (argc != 6) { fprintf(stderr, "usage: filter-oracle name enc|dec n in out\n"); return 2; }
  {
  const char *name = argv[1];
  const int enc = strcmp(argv[2], "enc") == 0;
  const UInt32 n = (UInt32)strtoul(argv[3], NULL, 0);
  FILE *fi = fopen(argv[4], "rb"), *fo;
  Byte *buf; size_t size;
  if (!fi) { perror("open in"); return 2; }
  fseek(fi, 0, SEEK_END); size = (size_t)ftell(fi); fseek(fi, 0, SEEK_SET);
  buf = (Byte *)malloc(size ? size : 1);
  if (!buf) return 2;
  if (size && fread(buf, 1, size, fi) != size) { perror("read"); return 2; }
  fclose(fi);

  if (strcmp(name, "delta") == 0)
  {
    Byte state[DELTA_STATE_SIZE];
    Delta_Init(state);
    if (enc) Delta_Encode(state, (unsigned)n, buf, size);
    else     Delta_Decode(state, (unsigned)n, buf, size);
  }
  else if (strcmp(name, "x86") == 0)
  {
    UInt32 state = Z7_BRANCH_CONV_ST_X86_STATE_INIT_VAL;
    if (enc) z7_BranchConvSt_X86_Enc(buf, size, n, &state);
    else     z7_BranchConvSt_X86_Dec(buf, size, n, &state);
  }
#define CONV(s, id) \
  else if (strcmp(name, s) == 0) \
  { if (enc) z7_BranchConv_ ## id ## _Enc(buf, size, n); \
    else     z7_BranchConv_ ## id ## _Dec(buf, size, n); }
  CONV("ppc",   PPC)
  CONV("ia64",  IA64)
  CONV("arm",   ARM)
  CONV("armt",  ARMT)
  CONV("sparc", SPARC)
  CONV("arm64", ARM64)
  CONV("riscv", RISCV)
  else { fprintf(stderr, "unknown filter %s\n", name); return 2; }

  fo = fopen(argv[5], "wb");
  if (!fo) { perror("open out"); return 2; }
  if (size && fwrite(buf, 1, size, fo) != size) { perror("write"); return 2; }
  fclose(fo);
  free(buf);
  return 0;
  }
}
"##;

/// The SDK's BCJ2 converter, driven from the command line, so the four-stream
/// port can be compared byte for byte against it.
///
/// Both directions are one call each: the buffers are sized so that neither
/// `Bcj2Enc_Encode` nor `Bcj2Dec_Decode` can run out of room, which is the
/// shape the reference documents as "decode full stream via single call".
const ORACLE_BCJ2_C: &str = r##"/* The SDK's BCJ2 converter, for parity testing.
   Usage: bcj2-oracle enc <relatLimit> <fileSize|-> <in> <main> <call> <jump> <rc>
          bcj2-oracle dec <origSize> <main> <call> <jump> <rc> <out> */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "Bcj2.h"

static Byte *slurp(const char *path, size_t *size)
{
  FILE *f = fopen(path, "rb");
  Byte *buf;
  if (!f) { perror(path); exit(2); }
  fseek(f, 0, SEEK_END); *size = (size_t)ftell(f); fseek(f, 0, SEEK_SET);
  buf = (Byte *)malloc(*size ? *size : 1);
  if (!buf) exit(2);
  if (*size && fread(buf, 1, *size, f) != *size) { perror("read"); exit(2); }
  fclose(f);
  return buf;
}

static void spit(const char *path, const Byte *p, size_t n)
{
  FILE *f = fopen(path, "wb");
  if (!f) { perror(path); exit(2); }
  if (n && fwrite(p, 1, n, f) != n) { perror("write"); exit(2); }
  fclose(f);
}

int main(int argc, char **argv)
{
  if (argc < 2) { fprintf(stderr, "usage: bcj2-oracle enc|dec ...\n"); return 2; }

  if (strcmp(argv[1], "enc") == 0)
  {
    size_t size, cap, cap4;
    Byte *src, *bufs[BCJ2_NUM_STREAMS];
    CBcj2Enc e;
    unsigned i;
    if (argc != 9) { fprintf(stderr, "usage: bcj2-oracle enc relatLimit fileSize in main call jump rc\n"); return 2; }
    src = slurp(argv[4], &size);
    cap = size + 1024;
    cap4 = (cap + 3) & ~(size_t)3;
    Bcj2Enc_Init(&e);
    e.relatLimit = (UInt32)strtoul(argv[2], NULL, 0);
    if (strcmp(argv[3], "-") != 0)
    {
      const CBcj2Enc_ip_unsigned fileSize =
          (CBcj2Enc_ip_unsigned)strtoull(argv[3], NULL, 0);
      Bcj2Enc_SET_FileSize(&e, fileSize)
    }
    for (i = 0; i < BCJ2_NUM_STREAMS; i++)
    {
      const size_t n = (i == BCJ2_STREAM_MAIN) ? cap : cap4;
      bufs[i] = (Byte *)malloc(n ? n : 4);
      if (!bufs[i]) return 2;
      e.bufs[i] = bufs[i];
      e.lims[i] = bufs[i] + n;
    }
    e.src = src;
    e.srcLim = src + size;
    e.finishMode = BCJ2_ENC_FINISH_MODE_END_STREAM;
    Bcj2Enc_Encode(&e);
    if (e.state != BCJ2_ENC_STATE_FINISHED || !Bcj2Enc_IsFinished(&e))
    {
      fprintf(stderr, "bcj2-oracle: encoder did not finish (state=%u)\n", e.state);
      return 3;
    }
    for (i = 0; i < BCJ2_NUM_STREAMS; i++)
      spit(argv[5 + i], bufs[i], (size_t)(e.bufs[i] - bufs[i]));
    return 0;
  }

  if (strcmp(argv[1], "dec") == 0)
  {
    size_t orig, sizes[BCJ2_NUM_STREAMS];
    Byte *bufs[BCJ2_NUM_STREAMS], *dest;
    CBcj2Dec d;
    unsigned i;
    SRes res;
    if (argc != 8) { fprintf(stderr, "usage: bcj2-oracle dec origSize main call jump rc out\n"); return 2; }
    orig = (size_t)strtoull(argv[2], NULL, 0);
    Bcj2Dec_Init(&d);
    for (i = 0; i < BCJ2_NUM_STREAMS; i++)
    {
      bufs[i] = slurp(argv[3 + i], &sizes[i]);
      d.bufs[i] = bufs[i];
      d.lims[i] = bufs[i] + sizes[i];
    }
    dest = (Byte *)malloc(orig ? orig : 1);
    if (!dest) return 2;
    d.dest = dest;
    d.destLim = dest + orig;
    res = Bcj2Dec_Decode(&d);
    if (res != SZ_OK) { fprintf(stderr, "bcj2-oracle: decode res=%d\n", res); return 4; }
    if ((size_t)(d.dest - dest) != orig)
    {
      fprintf(stderr, "bcj2-oracle: produced %u of %u bytes\n",
          (unsigned)(d.dest - dest), (unsigned)orig);
      return 5;
    }
    spit(argv[7], dest, orig);
    return 0;
  }

  fprintf(stderr, "unknown mode %s\n", argv[1]);
  return 2;
}
"##;

/// A props-driven LZMA-Alone encoder over the pinned SDK. It is written out by
/// [`lzma_util`] rather than committed, so that nothing in this repository
/// carries a copy of the reference sources.
const ORACLE_C: &str = r#"/* Props-driven LZMA-Alone encoder over the pinned SDK, for parity testing.
   Usage: oracle <level> <btMode> <numHashBytes> <lc> <lp> <pb> <fb> <dictSize> <in> <out> */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "LzmaEnc.h"
#include "Alloc.h"

typedef struct { ISeqInStream vt; const Byte *p; size_t rem; } MemIn;
static SRes MemIn_Read(ISeqInStreamPtr pp, void *buf, size_t *size) {
  MemIn *s = Z7_CONTAINER_FROM_VTBL(pp, MemIn, vt);
  size_t n = *size; if (n > s->rem) n = s->rem;
  memcpy(buf, s->p, n); s->p += n; s->rem -= n; *size = n; return SZ_OK;
}
typedef struct { ISeqOutStream vt; FILE *f; } FileOut;
static size_t FileOut_Write(ISeqOutStreamPtr pp, const void *buf, size_t size) {
  FileOut *s = Z7_CONTAINER_FROM_VTBL(pp, FileOut, vt);
  return fwrite(buf, 1, size, s->f);
}

int main(int argc, char **argv) {
  if (argc != 11) { fprintf(stderr, "bad args\n"); return 2; }
  CLzmaEncProps props; LzmaEncProps_Init(&props);
  props.level = atoi(argv[1]);
  props.btMode = atoi(argv[2]);
  props.numHashBytes = atoi(argv[3]);
  props.lc = atoi(argv[4]); props.lp = atoi(argv[5]); props.pb = atoi(argv[6]);
  props.fb = atoi(argv[7]);
  props.dictSize = (UInt32)strtoul(argv[8], NULL, 10);

  FILE *fi = fopen(argv[9], "rb"); if (!fi) return 3;
  fseek(fi, 0, SEEK_END); long n = ftell(fi); fseek(fi, 0, SEEK_SET);
  Byte *src = (Byte *)malloc((size_t)n + 1);
  if (n && fread(src, 1, (size_t)n, fi) != (size_t)n) return 3;
  fclose(fi);

  FILE *fo = fopen(argv[10], "wb"); if (!fo) return 3;
  CLzmaEncHandle enc = LzmaEnc_Create(&g_Alloc);
  if (!enc) return 4;
  if (LzmaEnc_SetProps(enc, &props) != SZ_OK) { fprintf(stderr, "setprops\n"); return 5; }

  Byte header[LZMA_PROPS_SIZE + 8]; size_t hs = LZMA_PROPS_SIZE;
  if (LzmaEnc_WriteProperties(enc, header, &hs) != SZ_OK) return 6;
  for (int i = 0; i < 8; i++) header[hs++] = (Byte)((UInt64)n >> (8 * i));
  fwrite(header, 1, hs, fo);

  MemIn in; in.vt.Read = MemIn_Read; in.p = src; in.rem = (size_t)n;
  FileOut out; out.vt.Write = FileOut_Write; out.f = fo;
  SRes res = LzmaEnc_Encode(enc, &out.vt, &in.vt, NULL, &g_Alloc, &g_Alloc);
  LzmaEnc_Destroy(enc, &g_Alloc, &g_Alloc);
  fclose(fo);
  if (res != SZ_OK) { fprintf(stderr, "encode res=%d\n", res); return 7; }
  return 0;
}
"#;

/// [`ORACLE_C`] again, built *without* `Z7_ST` so that `LzFindMt.c` is
/// compiled in, and taking the thread count that turns it on.
const ORACLE_MT_C: &str = r#"/* Props-driven LZMA-Alone encoder over the pinned SDK, built with the
   threaded match finder, for parity testing.
   Usage: lzma-oracle-mt <level> <btMode> <numHashBytes> <lc> <lp> <pb> <fb>
                         <dictSize> <numThreads> <in> <out> */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "LzmaEnc.h"
#include "Alloc.h"

typedef struct { ISeqInStream vt; const Byte *p; size_t rem; } MemIn;
static SRes MemIn_Read(ISeqInStreamPtr pp, void *buf, size_t *size) {
  MemIn *s = Z7_CONTAINER_FROM_VTBL(pp, MemIn, vt);
  size_t n = *size; if (n > s->rem) n = s->rem;
  memcpy(buf, s->p, n); s->p += n; s->rem -= n; *size = n; return SZ_OK;
}
typedef struct { ISeqOutStream vt; FILE *f; } FileOut;
static size_t FileOut_Write(ISeqOutStreamPtr pp, const void *buf, size_t size) {
  FileOut *s = Z7_CONTAINER_FROM_VTBL(pp, FileOut, vt);
  return fwrite(buf, 1, size, s->f);
}

int main(int argc, char **argv) {
  if (argc != 12) { fprintf(stderr, "bad args\n"); return 2; }
  CLzmaEncProps props; LzmaEncProps_Init(&props);
  props.level = atoi(argv[1]);
  props.btMode = atoi(argv[2]);
  props.numHashBytes = atoi(argv[3]);
  props.lc = atoi(argv[4]); props.lp = atoi(argv[5]); props.pb = atoi(argv[6]);
  props.fb = atoi(argv[7]);
  props.dictSize = (UInt32)strtoul(argv[8], NULL, 10);
  props.numThreads = atoi(argv[9]);

  FILE *fi = fopen(argv[10], "rb"); if (!fi) return 3;
  fseek(fi, 0, SEEK_END); long n = ftell(fi); fseek(fi, 0, SEEK_SET);
  Byte *src = (Byte *)malloc((size_t)n + 1);
  if (n && fread(src, 1, (size_t)n, fi) != (size_t)n) return 3;
  fclose(fi);

  FILE *fo = fopen(argv[11], "wb"); if (!fo) return 3;
  CLzmaEncHandle enc = LzmaEnc_Create(&g_Alloc);
  if (!enc) return 4;
  if (LzmaEnc_SetProps(enc, &props) != SZ_OK) { fprintf(stderr, "setprops\n"); return 5; }

  Byte header[LZMA_PROPS_SIZE + 8]; size_t hs = LZMA_PROPS_SIZE;
  if (LzmaEnc_WriteProperties(enc, header, &hs) != SZ_OK) return 6;
  for (int i = 0; i < 8; i++) header[hs++] = (Byte)((UInt64)n >> (8 * i));
  fwrite(header, 1, hs, fo);

  MemIn in; in.vt.Read = MemIn_Read; in.p = src; in.rem = (size_t)n;
  FileOut out; out.vt.Write = FileOut_Write; out.f = fo;
  SRes res = LzmaEnc_Encode(enc, &out.vt, &in.vt, NULL, &g_Alloc, &g_Alloc);
  LzmaEnc_Destroy(enc, &g_Alloc, &g_Alloc);
  fclose(fo);
  if (res != SZ_OK) { fprintf(stderr, "encode res=%d\n", res); return 7; }
  return 0;
}
"#;

/// The LZMA2 counterpart of [`ORACLE_C`], written out the same way.
const ORACLE2_C: &str = r#"/* Props-driven LZMA2 encoder over the pinned SDK, for parity testing. It
   writes the single LZMA2 property byte, then the raw LZMA2 stream.
   Usage: lzma2-oracle <level> <btMode> <numHashBytes> <lc> <lp> <pb> <fb> <dictSize> <in> <out> */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "Lzma2Enc.h"
#include "Alloc.h"

typedef struct { ISeqInStream vt; const Byte *p; size_t rem; } MemIn;
static SRes MemIn_Read(ISeqInStreamPtr pp, void *buf, size_t *size) {
  MemIn *s = Z7_CONTAINER_FROM_VTBL(pp, MemIn, vt);
  size_t n = *size; if (n > s->rem) n = s->rem;
  memcpy(buf, s->p, n); s->p += n; s->rem -= n; *size = n; return SZ_OK;
}
typedef struct { ISeqOutStream vt; FILE *f; } FileOut;
static size_t FileOut_Write(ISeqOutStreamPtr pp, const void *buf, size_t size) {
  FileOut *s = Z7_CONTAINER_FROM_VTBL(pp, FileOut, vt);
  return fwrite(buf, 1, size, s->f);
}

int main(int argc, char **argv) {
  if (argc != 11) { fprintf(stderr, "bad args\n"); return 2; }
  CLzma2EncProps props; Lzma2EncProps_Init(&props);
  props.lzmaProps.level = atoi(argv[1]);
  props.lzmaProps.btMode = atoi(argv[2]);
  props.lzmaProps.numHashBytes = atoi(argv[3]);
  props.lzmaProps.lc = atoi(argv[4]);
  props.lzmaProps.lp = atoi(argv[5]);
  props.lzmaProps.pb = atoi(argv[6]);
  props.lzmaProps.fb = atoi(argv[7]);
  props.lzmaProps.dictSize = (UInt32)strtoul(argv[8], NULL, 10);
  /* One solid block: this port has no block threads. */
  props.blockSize = LZMA2_ENC_PROPS_BLOCK_SIZE_SOLID;
  props.numBlockThreads_Max = 1;
  props.numBlockThreads_Reduced = 1;
  props.numTotalThreads = 1;
  props.lzmaProps.numThreads = 1;

  FILE *fi = fopen(argv[9], "rb"); if (!fi) return 3;
  fseek(fi, 0, SEEK_END); long n = ftell(fi); fseek(fi, 0, SEEK_SET);
  Byte *src = (Byte *)malloc((size_t)n + 1);
  if (n && fread(src, 1, (size_t)n, fi) != (size_t)n) return 3;
  fclose(fi);

  FILE *fo = fopen(argv[10], "wb"); if (!fo) return 3;
  CLzma2EncHandle enc = Lzma2Enc_Create(&g_Alloc, &g_Alloc);
  if (!enc) return 4;
  if (Lzma2Enc_SetProps(enc, &props) != SZ_OK) { fprintf(stderr, "setprops\n"); return 5; }
  Lzma2Enc_SetDataSize(enc, (UInt64)n);
  Byte prop = Lzma2Enc_WriteProperties(enc);
  fwrite(&prop, 1, 1, fo);

  MemIn in; in.vt.Read = MemIn_Read; in.p = src; in.rem = (size_t)n;
  FileOut out; out.vt.Write = FileOut_Write; out.f = fo;
  SRes res = Lzma2Enc_Encode2(enc, &out.vt, NULL, NULL, &in.vt, NULL, 0, NULL);
  Lzma2Enc_Destroy(enc);
  fclose(fo);
  if (res != SZ_OK) { fprintf(stderr, "encode res=%d\n", res); return 7; }
  return 0;
}
"#;

/// The LZMA2 oracle again, built without `Z7_ST` so that `MtCoder.c` and
/// `LzFindMt.c` are compiled in, and driven at a given block size, block
/// thread count and match-finder thread count.
const ORACLE2_MT_C: &str = r#"/* Props-driven multi-threaded LZMA2 encoder over the pinned SDK, for parity
   testing. It writes the single LZMA2 property byte, then the raw LZMA2
   stream. A blockSize of 0 means SOLID.
   Usage: lzma2-oracle-mt <level> <btMode> <numHashBytes> <lc> <lp> <pb> <fb>
                          <dictSize> <blockSize> <blockThreads> <mfThreads>
                          <in> <out> */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "Lzma2Enc.h"
#include "Alloc.h"

typedef struct { ISeqOutStream vt; FILE *f; } FileOut;
static size_t FileOut_Write(ISeqOutStreamPtr pp, const void *buf, size_t size) {
  FileOut *s = Z7_CONTAINER_FROM_VTBL(pp, FileOut, vt);
  return fwrite(buf, 1, size, s->f);
}

int main(int argc, char **argv) {
  if (argc != 14) { fprintf(stderr, "bad args\n"); return 2; }
  CLzma2EncProps props; Lzma2EncProps_Init(&props);
  props.lzmaProps.level = atoi(argv[1]);
  props.lzmaProps.btMode = atoi(argv[2]);
  props.lzmaProps.numHashBytes = atoi(argv[3]);
  props.lzmaProps.lc = atoi(argv[4]);
  props.lzmaProps.lp = atoi(argv[5]);
  props.lzmaProps.pb = atoi(argv[6]);
  props.lzmaProps.fb = atoi(argv[7]);
  props.lzmaProps.dictSize = (UInt32)strtoul(argv[8], NULL, 10);
  {
    UInt64 blockSize = (UInt64)strtoull(argv[9], NULL, 10);
    props.blockSize = blockSize ? blockSize : LZMA2_ENC_PROPS_BLOCK_SIZE_SOLID;
  }
  props.numBlockThreads_Max = atoi(argv[10]);
  props.numBlockThreads_Reduced = props.numBlockThreads_Max;
  props.lzmaProps.numThreads = atoi(argv[11]);
  props.numTotalThreads = props.numBlockThreads_Max * props.lzmaProps.numThreads;

  FILE *fi = fopen(argv[12], "rb"); if (!fi) return 3;
  fseek(fi, 0, SEEK_END); long n = ftell(fi); fseek(fi, 0, SEEK_SET);
  Byte *src = (Byte *)malloc((size_t)n + 1);
  if (n && fread(src, 1, (size_t)n, fi) != (size_t)n) return 3;
  fclose(fi);

  FILE *fo = fopen(argv[13], "wb"); if (!fo) return 3;
  CLzma2EncHandle enc = Lzma2Enc_Create(&g_Alloc, &g_Alloc);
  if (!enc) return 4;
  if (Lzma2Enc_SetProps(enc, &props) != SZ_OK) { fprintf(stderr, "setprops\n"); return 5; }
  Lzma2Enc_SetDataSize(enc, (UInt64)n);
  Byte prop = Lzma2Enc_WriteProperties(enc);
  fwrite(&prop, 1, 1, fo);

  FileOut out; out.vt.Write = FileOut_Write; out.f = fo;
  SRes res = Lzma2Enc_Encode2(enc, &out.vt, NULL, NULL, NULL, src, (size_t)n, NULL);
  Lzma2Enc_Destroy(enc);
  fclose(fo);
  if (res != SZ_OK) { fprintf(stderr, "encode res=%d\n", res); return 7; }
  return 0;
}
"#;

/// The SHA-256 of `data`, as lowercase hex, for comparing a download with the
/// digest pinned above.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
