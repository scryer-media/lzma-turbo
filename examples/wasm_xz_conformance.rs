//! Conformance harness for the wasm host hooks, and the reference embedding
//! of the [`lzma_turbo::hooks`] seam for a core wasm module.
//!
//! This is the guest half of the `crc-host` + `crypto-host` conformance test.
//! Built for `wasm32-wasip1` with `--no-default-features --features
//! std,crc-host,crypto-host,xz`, it installs hooks that forward to raw imports
//! it declares itself in a `host` namespace (see the `embedding` module below),
//! then decodes one `.xz` fixture per check type so that:
//!
//!   * every CRC-32 the container carries - the mandatory stream and block
//!     header CRCs, the index CRC, the footer CRC, and the check of a
//!     `--check=crc32` stream - crosses the boundary to `host::host_crc32`,
//!   * a `--check=crc64` stream's block check crosses to `host::host_crc64_xz`,
//!   * a `--check=sha256` stream's block check is hashed by the host's
//!     streaming SHA-256, opened, fed and closed through the handle contract,
//!   * and the report itself is fingerprinted with `lzma_turbo::crypto::Sha256`,
//!     which on this build is the host's hash too.
//!
//! The LZMA2 decode, the filters and the framing all stay in-wasm. Only the
//! bulk checksums are delegated.
//!
//! Every case prints one line, and the lines are the conformance claim: the
//! native driver in `tools/wasm-conformance/tests/wasm_host_conformance.rs` runs this same program
//! natively - where the delegating features are inert, so the in-process
//! `crc-fast` and `sha2`/AWS-LC backends do the work and no hook is called -
//! and requires the two reports to be byte for byte identical. So the decoded
//! bytes AND the verdicts, including the failure text of every corrupted
//! stream, must match the native decoder exactly.
//!
//! Build (wasm):
//!   cargo build --release -p lzma-turbo --no-default-features \
//!     --features std,crc-host,crypto-host,xz \
//!     --target wasm32-wasip1 --example wasm_xz_conformance
//!
//! It is not meaningful under a bare `wasmtime` CLI: the `host` imports are
//! unsatisfied there and instantiation fails. Run it through the driver:
//!   cargo test -p wasm-conformance
//!
//! Natively, for debugging (features inert, no hook called):
//!   cargo run -p lzma-turbo --features crc-host,crypto-host \
//!     --example wasm_xz_conformance -- tests/data

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use lzma_turbo::XzReader;
use lzma_turbo::crypto::Sha256;

/// The example's own raw imports and the hooks that forward to them.
///
/// ABI (a fixed contract, shared with the driver in
/// `tools/wasm-conformance/tests/wasm_host_conformance.rs`; this crate itself knows none of it and
/// only ever sees the `fn` pointers):
///
/// ```text
/// host_crc32(seed: i32, ptr: i32, len: i32) -> i32
/// host_crc64_xz(seed: i64, ptr: i32, len: i32) -> i64
/// host_sha256_init() -> i64
/// host_sha256_clone(handle: i64) -> i64
/// host_sha256_update(handle: i64, ptr: i32, len: i32)
/// host_sha256_finalize(handle: i64, out_ptr: i32)
/// host_sha256_drop(handle: i64)
/// ```
///
/// Every `ptr` is a byte offset into this module's linear memory, which the
/// host reads (or, for `out_ptr`, writes) in place - the zero-copy shape a
/// core wasm module can offer. `host_sha256_finalize` writes exactly 32 bytes
/// at `out_ptr`. A bad offset or a stale handle is a contract violation and
/// the host traps rather than returning a wrong answer; there is no error
/// code, because there is no error a guest could do anything about.
///
/// The CRC seeds and results are in the finalized domain and chain, exactly as
/// [`lzma_turbo::hooks`] requires.
#[cfg(target_arch = "wasm32")]
mod embedding {
    use lzma_turbo::hooks::{HostHashHooks, HostSha256Handle, install_host_hash_hooks};

    #[link(wasm_import_module = "host")]
    unsafe extern "C" {
        fn host_crc32(seed: i32, ptr: i32, len: i32) -> i32;
        fn host_crc64_xz(seed: i64, ptr: i32, len: i32) -> i64;
        fn host_sha256_init() -> i64;
        fn host_sha256_clone(handle: i64) -> i64;
        fn host_sha256_update(handle: i64, ptr: i32, len: i32);
        fn host_sha256_finalize(handle: i64, out_ptr: i32);
        fn host_sha256_drop(handle: i64);
    }

    fn crc32(seed: u32, data: &[u8]) -> u32 {
        // SAFETY: `data.as_ptr()`/`data.len()` are a valid read-only
        // offset+length into this module's own linear memory; the host slices
        // them in place and never retains them past the call.
        unsafe { host_crc32(seed as i32, data.as_ptr() as i32, data.len() as i32) as u32 }
    }

    fn crc64_xz(seed: u64, data: &[u8]) -> u64 {
        // SAFETY: as in `crc32` - a read-only slice of this module's memory,
        // borrowed only for the duration of the call.
        unsafe { host_crc64_xz(seed as i64, data.as_ptr() as i32, data.len() as i32) as u64 }
    }

    fn sha256_init() -> HostSha256Handle {
        // SAFETY: no memory is shared; the host allocates its own state.
        HostSha256Handle(unsafe { host_sha256_init() } as u64)
    }

    fn sha256_clone(handle: HostSha256Handle) -> HostSha256Handle {
        // SAFETY: `handle` is live - this crate passes each handle to exactly
        // one of finalize/drop, exactly once - so the host can resolve it.
        HostSha256Handle(unsafe { host_sha256_clone(handle.0 as i64) } as u64)
    }

    fn sha256_update(handle: HostSha256Handle, data: &[u8]) {
        // SAFETY: a live handle, plus a read-only slice of this module's own
        // memory borrowed only for the call.
        unsafe { host_sha256_update(handle.0 as i64, data.as_ptr() as i32, data.len() as i32) };
    }

    fn sha256_finalize(handle: HostSha256Handle) -> [u8; 32] {
        let mut out = [0u8; 32];
        // SAFETY: a live handle, and `out` is 32 writable bytes in this
        // module's memory, which is exactly what the host writes.
        unsafe { host_sha256_finalize(handle.0 as i64, out.as_mut_ptr() as i32) };
        out
    }

    fn sha256_drop(handle: HostSha256Handle) {
        // SAFETY: a live handle, consumed here and never passed again.
        unsafe { host_sha256_drop(handle.0 as i64) };
    }

    pub(super) fn install() {
        install_host_hash_hooks(HostHashHooks::new(
            crc32,
            crc64_xz,
            sha256_init,
            sha256_clone,
            sha256_update,
            sha256_finalize,
            sha256_drop,
        ));
    }
}

/// One fixture: a stream of the same source bytes under one check type.
struct Case {
    /// How the report names it.
    label: &'static str,
    /// The `.xz` file, relative to the fixture root.
    stream: &'static str,
    /// The bytes it must decode to, relative to the fixture root.
    plain: &'static str,
    /// Length of the block's check field, 0 for `--check=none`. A stream with
    /// a check also gets the corruption case below.
    check_len: usize,
}

/// One fixture per check type the xz container defines, all over the same
/// source file, so a difference between them is a difference in the check and
/// nothing else.
const CASES: &[Case] = &[
    Case {
        label: "crc32 ",
        stream: "mixed.crc32.xz",
        plain: "src_mixed.bin",
        check_len: 4,
    },
    Case {
        label: "crc64 ",
        stream: "mixed.p1.xz",
        plain: "src_mixed.bin",
        check_len: 8,
    },
    Case {
        label: "sha256",
        stream: "mixed.sha256.xz",
        plain: "src_mixed.bin",
        check_len: 32,
    },
    Case {
        label: "none  ",
        stream: "mixed.nocheck.xz",
        plain: "src_mixed.bin",
        check_len: 0,
    },
];

/// Where the block's check field ends in a single-block stream: at the first
/// byte of the index.
///
/// The footer is the last 12 bytes - `CRC32(4) | backward_size(4) |
/// stream_flags(2) | "YZ"` - and `backward_size` encodes the index length as
/// `(backward_size + 1) * 4`. So the index begins that far before the footer,
/// and the check field is the `check_len` bytes immediately before it. This is
/// how the corruption case can hit the stored check *exactly*, and so provoke
/// a check mismatch rather than a decode failure somewhere upstream of it.
fn index_start(stream: &[u8]) -> Option<usize> {
    let footer = stream.len().checked_sub(12)?;
    if &stream[stream.len() - 2..] != b"YZ" {
        return None;
    }
    let backward = u32::from_le_bytes(stream[footer + 4..footer + 8].try_into().ok()?);
    let index_size = (u64::from(backward) + 1).checked_mul(4)?;
    footer.checked_sub(usize::try_from(index_size).ok()?)
}

/// Decode a whole `.xz` stream, returning either the bytes or the reader's
/// verdict as the text a user would see.
fn decode(stream: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match XzReader::new(stream).read_to_end(&mut out) {
        Ok(_) => Ok(out),
        Err(e) => Err(e.to_string()),
    }
}

/// Lowercase hex, for a report line.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// SHA-256 of a buffer through `lzma_turbo::crypto`, which on a `crypto-host`
/// wasm build is the embedder's hash, opened and closed through the handle
/// contract. Fed in small pieces on purpose: a one-shot would never exercise
/// the streaming part of that contract.
fn fingerprint(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    for chunk in bytes.chunks(4096) {
        h.update(chunk);
    }
    hex(&h.finalize())
}

fn read(root: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = root.join(name);
    std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// Build the report for one case: the clean decode, then - for a stream that
/// has a check - the same stream with the last byte of its stored check
/// flipped, which must be refused.
fn report_case(root: &Path, case: &Case, out: &mut Vec<String>) -> bool {
    let stream = match read(root, case.stream) {
        Ok(b) => b,
        Err(e) => {
            out.push(format!("FAIL | {} | {e}", case.label));
            return false;
        }
    };
    let plain = match read(root, case.plain) {
        Ok(b) => b,
        Err(e) => {
            out.push(format!("FAIL | {} | {e}", case.label));
            return false;
        }
    };

    let mut ok = true;

    match decode(&stream) {
        Ok(bytes) if bytes == plain => out.push(format!(
            "PASS | {} | decode | {:>8} bytes | sha256 {}",
            case.label,
            bytes.len(),
            fingerprint(&bytes)
        )),
        Ok(bytes) => {
            ok = false;
            out.push(format!(
                "FAIL | {} | decode | {} bytes, expected {} | sha256 {}",
                case.label,
                bytes.len(),
                plain.len(),
                fingerprint(&bytes)
            ));
        }
        Err(e) => {
            ok = false;
            out.push(format!("FAIL | {} | decode | refused: {e}", case.label));
        }
    }

    if case.check_len == 0 {
        out.push(format!("PASS | {} | corrupt-check | n/a", case.label));
        return ok;
    }

    let Some(index) = index_start(&stream) else {
        out.push(format!(
            "FAIL | {} | corrupt-check | could not locate the index",
            case.label
        ));
        return false;
    };
    let mut bad = stream.clone();
    bad[index - 1] ^= 0x01;
    match decode(&bad) {
        Ok(_) => {
            ok = false;
            out.push(format!(
                "FAIL | {} | corrupt-check | a flipped check byte went unnoticed",
                case.label
            ));
        }
        Err(e) => out.push(format!("PASS | {} | corrupt-check | {e}", case.label)),
    }

    ok
}

fn main() {
    #[cfg(target_arch = "wasm32")]
    embedding::install();

    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/fixtures"));

    // Stderr, not stdout: the driver compares the guest's stdout with the
    // native run's byte for byte, and this line is the one thing that must
    // differ between them.
    eprintln!(
        "wasm_xz_conformance: root={} crc-delegated={} sha256-delegated={}",
        root.display(),
        cfg!(all(target_arch = "wasm32", feature = "crc-host")),
        lzma_turbo::crypto::SHA256_IS_HOST_DELEGATED,
    );

    let mut lines = Vec::new();
    let mut failed = 0usize;
    for case in CASES {
        if !report_case(&root, case, &mut lines) {
            failed += 1;
        }
    }

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "==== xz check conformance ====");
    for line in &lines {
        let _ = writeln!(stdout, "{line}");
    }
    let _ = writeln!(stdout, "==============================");
    let _ = writeln!(stdout, "cases={} failed={failed}", CASES.len());
    // WASI aborts do not flush libc stdio, so flush explicitly: the report is
    // the result, and losing it would turn a diagnosable failure into a trap.
    let _ = stdout.flush();

    if failed != 0 {
        std::process::exit(1);
    }
}
