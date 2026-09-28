//! Native `wasmtime` driver for the wasm host-hook conformance examples.
//!
//! This is the executable proof that `.xz` decoding runs correctly through the
//! embedder hooks - the `crc-host` CRC-32 and CRC-64/XZ AND the `crypto-host`
//! streaming SHA-256 (see `lzma_turbo::hooks`) - as wired by
//! `examples/wasm_xz_conformance.rs`, whose hooks forward to its own raw
//! imports in a `host` namespace. It doubles as the reference implementation of
//! that ABI: the host functions below satisfy it exactly, with `crc-fast` and
//! `sha2` doing the arithmetic, so there is no hand-written checksum on either
//! side of the boundary.
//!
//! The conformance claim is stdout equality. The same example is run twice -
//! once as a `wasm32-wasip1` guest with every bulk checksum crossing to the
//! host, once natively with the delegating features inert and the in-process
//! backends doing the work - and the two reports must be byte for byte
//! identical. Decoded bytes, their SHA-256 fingerprints, and the verdict text
//! for every corrupted stream all have to match the native decoder.
//!
//! A second case covers the other half of the contract: a guest that installs
//! no hooks must panic with the documented message rather than fall back to an
//! in-guest checksum.
//!
//! Flow:
//!   1. Build the example for `wasm32-wasip1` with `--no-default-features
//!      --features std,crc-host,crypto-host,xz`, in a private target dir so the
//!      nested cargo does not fight the outer test's target lock.
//!   2. Instantiate with `wasmtime`, providing WASI preview1 (stdio + argv +
//!      `tests/data` preopened read-only as `/fixtures`) plus the seven custom
//!      host imports.
//!   3. Run `_start`, capture stdout, and compare it with the native run's.
//!
//! Skipped automatically if no reachable toolchain has a `wasm32-wasip1` std.

#![cfg(not(target_family = "wasm"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use sha2::Digest as _;
use wasmtime::{Caller, Engine, Extern, Linker, Memory, Module, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

// ---------------------------------------------------------------------------
// The host side of the ABI. Every function here is the contract in
// `lzma_turbo::hooks` made concrete for a core wasm module: the CRCs are
// seeded resumes in the finalized domain, and the SHA-256 is a streaming state
// behind an opaque handle whose lifetime the guest is required to close.
// ---------------------------------------------------------------------------

/// The host's SHA-256 states, keyed by the handle the guest holds.
///
/// Handles are never reused, so a guest that finalized or dropped one and then
/// used it again would find nothing here and trap - which is exactly the
/// failure mode the contract asks a host to have, rather than quietly hashing
/// into a fresh state and returning a plausible wrong digest.
#[derive(Default)]
struct HostState {
    sha: HashMap<u64, sha2::Sha256>,
    next_handle: u64,
    /// How many handles were opened and how many closed, so the test can
    /// assert the guest leaked none.
    opened: u64,
    closed: u64,
}

impl HostState {
    fn open(&mut self, state: sha2::Sha256) -> u64 {
        self.next_handle += 1;
        self.opened += 1;
        let handle = self.next_handle;
        self.sha.insert(handle, state);
        handle
    }

    fn get(&mut self, handle: u64) -> &mut sha2::Sha256 {
        self.sha
            .get_mut(&handle)
            .unwrap_or_else(|| panic!("guest used a stale SHA-256 handle {handle}"))
    }

    fn close(&mut self, handle: u64) -> sha2::Sha256 {
        self.closed += 1;
        self.sha
            .remove(&handle)
            .unwrap_or_else(|| panic!("guest closed a SHA-256 handle {handle} twice"))
    }
}

/// What each wasm instance carries: the WASI context and the host's hash states.
struct Ctx {
    wasi: WasiP1Ctx,
    host: Arc<Mutex<HostState>>,
}

/// Read `len` bytes at `ptr` from the guest's linear memory, or panic - an
/// out-of-bounds offset is a contract violation, and the contract says the host
/// traps rather than inventing an answer.
fn guest_slice(memory: &Memory, caller: &Caller<'_, Ctx>, ptr: i32, len: i32) -> Vec<u8> {
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    let mut buf = vec![0u8; len];
    memory
        .read(caller, ptr, &mut buf)
        .unwrap_or_else(|e| panic!("guest passed an out-of-bounds slice {ptr}+{len}: {e}"));
    buf
}

fn memory_of(caller: &mut Caller<'_, Ctx>) -> Memory {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => m,
        _ => panic!("the guest module must export its linear memory as `memory`"),
    }
}

/// Resume a CRC from a finalized `seed`. Both algorithms are "init all ones,
/// final xor all ones", so `crc-fast`'s running register is the bitwise
/// complement of the finalized value in both directions - seeding the digest
/// with `!seed` continues exactly where a stream carrying `seed` left off.
fn resume_crc(algorithm: crc_fast::CrcAlgorithm, init_state: u64, data: &[u8]) -> u64 {
    let mut digest = crc_fast::Digest::new_with_init_state(algorithm, init_state);
    digest.update(data);
    digest.finalize()
}

fn host_crc32(mut caller: Caller<'_, Ctx>, seed: i32, ptr: i32, len: i32) -> i32 {
    let seed = seed as u32;
    if len == 0 {
        return seed as i32;
    }
    let memory = memory_of(&mut caller);
    let data = guest_slice(&memory, &caller, ptr, len);
    resume_crc(
        crc_fast::CrcAlgorithm::Crc32IsoHdlc,
        u64::from(!seed),
        &data,
    ) as u32 as i32
}

fn host_crc64_xz(mut caller: Caller<'_, Ctx>, seed: i64, ptr: i32, len: i32) -> i64 {
    let seed = seed as u64;
    if len == 0 {
        return seed as i64;
    }
    let memory = memory_of(&mut caller);
    let data = guest_slice(&memory, &caller, ptr, len);
    resume_crc(crc_fast::CrcAlgorithm::Crc64Xz, !seed, &data) as i64
}

fn host_sha256_init(caller: Caller<'_, Ctx>) -> i64 {
    let host = Arc::clone(&caller.data().host);
    let mut host = host.lock().expect("host state");
    host.open(sha2::Sha256::new()) as i64
}

fn host_sha256_clone(caller: Caller<'_, Ctx>, handle: i64) -> i64 {
    let host = Arc::clone(&caller.data().host);
    let mut host = host.lock().expect("host state");
    let copy = host.get(handle as u64).clone();
    host.open(copy) as i64
}

fn host_sha256_update(mut caller: Caller<'_, Ctx>, handle: i64, ptr: i32, len: i32) {
    if len == 0 {
        // Still resolve the handle: an empty update on a stale one is as much
        // a contract violation as any other.
        let host = Arc::clone(&caller.data().host);
        host.lock().expect("host state").get(handle as u64);
        return;
    }
    let memory = memory_of(&mut caller);
    let data = guest_slice(&memory, &caller, ptr, len);
    let host = Arc::clone(&caller.data().host);
    host.lock()
        .expect("host state")
        .get(handle as u64)
        .update(&data);
}

fn host_sha256_finalize(mut caller: Caller<'_, Ctx>, handle: i64, out_ptr: i32) {
    let digest = {
        let host = Arc::clone(&caller.data().host);
        let mut host = host.lock().expect("host state");
        host.close(handle as u64).finalize()
    };
    let memory = memory_of(&mut caller);
    memory
        .write(&mut caller, out_ptr as u32 as usize, digest.as_slice())
        .unwrap_or_else(|e| panic!("guest passed an unwritable 32-byte digest slot: {e}"));
}

fn host_sha256_drop(caller: Caller<'_, Ctx>, handle: i64) {
    let host = Arc::clone(&caller.data().host);
    host.lock().expect("host state").close(handle as u64);
}

// ---------------------------------------------------------------------------
// Toolchain discovery and the nested wasm build.
// ---------------------------------------------------------------------------

/// Does this `rustc` actually have the `wasm32-wasip1` standard library?
///
/// `--print target-list` is not a usable probe: it lists every target rustc can
/// name, installed or not. The target libdir existing is the real signal.
fn has_wasip1_std(rustc: &Path) -> bool {
    Command::new(rustc)
        .args(["--print", "target-libdir", "--target", "wasm32-wasip1"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| Path::new(String::from_utf8_lossy(&out.stdout).trim()).is_dir())
}

/// Resolve a `(cargo, rustc)` pair that can build `wasm32-wasip1`.
///
/// The ambient `cargo`/`rustc` are not necessarily the toolchain pinned by
/// `rust-toolchain.toml` - a Homebrew `rust` puts real binaries on PATH ahead
/// of rustup's proxies, and those carry no wasm std and are a different build
/// of the same version number, so their artifacts cannot be mixed (E0514).
/// Candidates are tried in order: `RUSTC`, the `rustc` beside the outer
/// `CARGO`, whatever `rustup which rustc` resolves for this manifest, then
/// PATH. The first with a real wasm std wins, and the nested build is pinned to
/// it.
fn wasm_toolchain() -> Option<(PathBuf, PathBuf)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(rustc) = std::env::var_os("RUSTC") {
        candidates.push(PathBuf::from(rustc));
    }
    if let Some(dir) = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .and_then(|cargo| cargo.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join("rustc"));
    }
    if let Some(out) = Command::new("rustup")
        .args(["which", "rustc"])
        .current_dir(workspace_root())
        .output()
        .ok()
        .filter(|out| out.status.success())
    {
        candidates.push(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()));
    }
    candidates.push(PathBuf::from("rustc"));

    let rustc = candidates.into_iter().find(|rustc| has_wasip1_std(rustc))?;
    let cargo = rustc
        .parent()
        .map(|dir| dir.join("cargo"))
        .filter(|cargo| cargo.is_file())
        .or_else(|| std::env::var_os("CARGO").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cargo"));
    Some((cargo, rustc))
}

/// The feature set a delegating wasm guest is built with: no defaults (which
/// would pull AWS-LC's C), `std` for the readers and the hook registry, and
/// both host features so every bulk checksum crosses the boundary.
const GUEST_FEATURES: &str = "std,crc-host,crypto-host,xz";

/// Build one example for `wasm32-wasip1` with [`GUEST_FEATURES`] and return the
/// `.wasm`. A private target dir keeps the nested cargo off the outer test's
/// target lock.
fn build_guest(example: &str) -> PathBuf {
    let manifest_dir = workspace_root();
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-host-conformance");
    let (cargo, rustc) =
        wasm_toolchain().expect("no toolchain with a wasm32-wasip1 std (checked by the caller)");

    let status = Command::new(&cargo)
        .current_dir(&manifest_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("RUSTC", &rustc)
        .env("RUSTDOC", rustc.with_file_name("rustdoc"))
        // Do not inherit the outer test's RUSTFLAGS.
        .env("RUSTFLAGS", "")
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "lzma-turbo",
            "--example",
            example,
            "--no-default-features",
            "--features",
            GUEST_FEATURES,
            "--target",
            "wasm32-wasip1",
        ])
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo to build the {example} guest: {e}"));
    assert!(
        status.success(),
        "cargo build of the {example} guest failed"
    );

    let wasm = target_dir
        .join("wasm32-wasip1")
        .join("release")
        .join("examples")
        .join(format!("{example}.wasm"));
    assert!(
        wasm.is_file(),
        "expected built wasm at {}, but it is missing",
        wasm.display()
    );
    wasm
}

/// The workspace root: where the nested cargo runs from, and where the guest
/// examples and fixtures live. This crate sits two directories below it.
fn workspace_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root
}

/// The fixture root both runs read: lzma-turbo's `tests/data`, the committed
/// `.xz` vectors and the source bytes they decode to.
fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("data")
}

/// What a run produced.
struct Run {
    stdout: String,
    stderr: String,
    /// `None` for a clean return, otherwise the process/WASI exit code.
    exit: Option<i32>,
}

/// Instantiate `wasm` under WASI with the seven host imports and run `_start`.
///
/// Returns the captured stdio and exit code, plus the host's handle ledger, so
/// the caller can assert the guest closed every SHA-256 state it opened.
fn run_guest(wasm: &Path, with_hooks: bool) -> (Run, (u64, u64)) {
    let engine = Engine::default();
    let module = Module::from_file(&engine, wasm).expect("load wasm module");

    let stdout = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let stderr = wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(1 << 20);
    let wasi = WasiCtxBuilder::new()
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .args(&["conformance", "/fixtures"])
        .preopened_dir(fixtures_dir(), "/fixtures", FsPerms::ReadOnly)
        .expect("preopen the fixture dir")
        .build_p1();

    let host = Arc::new(Mutex::new(HostState::default()));
    let mut store = Store::new(
        &engine,
        Ctx {
            wasi,
            host: Arc::clone(&host),
        },
    );

    let mut linker: Linker<Ctx> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |ctx: &mut Ctx| &mut ctx.wasi)
        .expect("add wasi preview1 to the linker");

    if with_hooks {
        // The fixed namespace the example's raw externs name.
        linker.func_wrap("host", "host_crc32", host_crc32).unwrap();
        linker
            .func_wrap("host", "host_crc64_xz", host_crc64_xz)
            .unwrap();
        linker
            .func_wrap("host", "host_sha256_init", host_sha256_init)
            .unwrap();
        linker
            .func_wrap("host", "host_sha256_clone", host_sha256_clone)
            .unwrap();
        linker
            .func_wrap("host", "host_sha256_update", host_sha256_update)
            .unwrap();
        linker
            .func_wrap("host", "host_sha256_finalize", host_sha256_finalize)
            .unwrap();
        linker
            .func_wrap("host", "host_sha256_drop", host_sha256_drop)
            .unwrap();
    }

    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate the guest (every import must be satisfied)");
    let start = instance
        .get_typed_func::<(), ()>(&mut store, "_start")
        .expect("a wasip1 command exports _start");

    let exit = match start.call(&mut store, ()) {
        Ok(()) => None,
        Err(err) => match err.downcast_ref::<wasmtime_wasi::I32Exit>() {
            Some(exit) => Some(exit.0),
            // A trap: an aborted panic under `panic = "abort"`. Report it as a
            // non-zero exit so the caller can tell success from failure
            // without caring which shape the runtime chose.
            None => Some(-1),
        },
    };

    drop(store);
    let ledger = {
        let host = host.lock().expect("host state");
        (host.opened, host.closed)
    };
    (
        Run {
            stdout: String::from_utf8_lossy(&stdout.contents()).into_owned(),
            stderr: String::from_utf8_lossy(&stderr.contents()).into_owned(),
            exit,
        },
        ledger,
    )
}

/// Run the same example natively, through the in-process backends, and return
/// its stdout. This is the ground truth the guest report must equal.
fn run_native(example: &str) -> Run {
    let manifest_dir = workspace_root();
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("native-conformance");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let out = Command::new(cargo)
        .current_dir(&manifest_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("RUSTFLAGS", "")
        .args([
            "run",
            "--locked",
            "--release",
            "-q",
            "-p",
            "lzma-turbo",
            "--example",
            example,
            "--features",
            "crc-host,crypto-host",
            "--",
        ])
        .arg(fixtures_dir())
        .output()
        .unwrap_or_else(|e| panic!("failed to run the native {example}: {e}"));

    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        exit: out.status.code().filter(|&c| c != 0),
    }
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

/// A `.xz` stream of each check type, decoded in a wasm guest whose CRC-32,
/// CRC-64/XZ and SHA-256 all live in the host, must produce exactly what the
/// native decoder produces - the same bytes, the same fingerprints, and the
/// same verdict on a stream whose stored check was corrupted.
#[test]
fn wasm_guest_matches_the_native_decoder() {
    if wasm_toolchain().is_none() {
        eprintln!("skipping: no toolchain with a wasm32-wasip1 std");
        return;
    }

    let wasm = build_guest("wasm_xz_conformance");
    let (guest, (opened, closed)) = run_guest(&wasm, true);
    let native = run_native("wasm_xz_conformance");

    assert_eq!(
        native.exit, None,
        "the native run failed; its report was:\n{}\n{}",
        native.stdout, native.stderr
    );
    assert_eq!(
        guest.exit, None,
        "the wasm guest failed; its report was:\n{}\n{}",
        guest.stdout, guest.stderr
    );
    assert_eq!(
        guest.stdout, native.stdout,
        "the delegating wasm guest and the native decoder disagree.\n\
         guest stderr:\n{}\nnative stderr:\n{}",
        guest.stderr, native.stderr
    );

    // The report is not vacuous: every case has to be in it and every one has
    // to have passed.
    assert!(
        guest.stdout.contains("cases=4 failed=0"),
        "unexpected summary line in:\n{}",
        guest.stdout
    );
    for label in ["crc32", "crc64", "sha256", "none"] {
        assert!(
            guest.stdout.contains(label),
            "case {label} missing from:\n{}",
            guest.stdout
        );
    }

    // The SHA-256 handles really were the host's, and the guest closed every
    // one it opened - the lifetime half of the contract.
    assert!(
        opened > 0,
        "no SHA-256 handle was ever opened, so the crypto-host seam was not exercised"
    );
    assert_eq!(
        opened,
        closed,
        "the guest leaked {} SHA-256 handle(s)",
        opened - closed
    );
}

/// A guest that installs no hooks must panic with the message `hooks` names,
/// not fall back to hashing in-guest. The same program natively is fine, which
/// is the "accepted but inert" half of the feature contract.
#[test]
fn wasm_guest_without_hooks_panics_with_the_documented_message() {
    if wasm_toolchain().is_none() {
        eprintln!("skipping: no toolchain with a wasm32-wasip1 std");
        return;
    }

    let wasm = build_guest("wasm_missing_hooks");
    // No host imports are linked, and the example declares none - the failure
    // has to come from the empty registry, not from a missing import.
    let (guest, _) = run_guest(&wasm, false);

    assert!(
        guest.exit.is_some_and(|code| code != 0),
        "a guest with no hooks must not succeed; stdout was:\n{}",
        guest.stdout
    );
    assert!(
        guest.stderr.contains("no host hash hooks installed"),
        "the panic must name the missing wiring; stderr was:\n{}",
        guest.stderr
    );
    assert!(
        guest
            .stderr
            .contains("lzma_turbo::hooks::install_host_hash_hooks"),
        "the panic must name the call to make; stderr was:\n{}",
        guest.stderr
    );
    assert!(
        !guest.stdout.contains("crc32="),
        "the guest computed a CRC without a host, so the seam fell back:\n{}",
        guest.stdout
    );

    // Natively the same program is inert and simply works.
    let native = run_native("wasm_missing_hooks");
    assert_eq!(
        native.exit, None,
        "natively the feature is accepted and inert; got:\n{}\n{}",
        native.stdout, native.stderr
    );
    assert!(
        native.stdout.contains("crc32="),
        "the native run should have computed a CRC in process:\n{}",
        native.stdout
    );
}

/// The reference host's CRC contract, on its own, so a regression in the
/// functions an embedder is meant to copy is caught even when the wasm lanes
/// self-skip: zero seeds the stream, an empty update is the identity, and a
/// seeded resume equals the whole stream.
#[test]
fn reference_host_crcs_satisfy_the_seed_contract() {
    let data: Vec<u8> = (0u32..4096).map(|i| (i * 37 + 11) as u8).collect();

    let crc32 = |seed: u32, d: &[u8]| {
        resume_crc(crc_fast::CrcAlgorithm::Crc32IsoHdlc, u64::from(!seed), d) as u32
    };
    let crc64 = |seed: u64, d: &[u8]| resume_crc(crc_fast::CrcAlgorithm::Crc64Xz, !seed, d);

    assert_eq!(crc32(0, &data), lzma_turbo::crc::crc32(&data));
    assert_eq!(crc64(0, &data), lzma_turbo::crc::crc64_xz(&data));
    assert_eq!(crc32(0, &[]), 0);
    assert_eq!(crc64(0, &[]), 0);
    assert_eq!(crc32(0xdead_beef, &[]), 0xdead_beef);
    assert_eq!(crc64(0xdead_beef_0bad_f00d, &[]), 0xdead_beef_0bad_f00d);

    for cut in [0usize, 1, 2, 17, 64, 1000, 4095, 4096] {
        let (a, b) = data.split_at(cut);
        assert_eq!(
            crc32(crc32(0, a), b),
            lzma_turbo::crc::crc32(&data),
            "crc32 chain, cut {cut}"
        );
        assert_eq!(
            crc64(crc64(0, a), b),
            lzma_turbo::crc::crc64_xz(&data),
            "crc64 chain, cut {cut}"
        );
    }
}
