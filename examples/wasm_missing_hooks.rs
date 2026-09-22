//! The other half of the `hooks` contract: what a guest that forgot to wire
//! the host does.
//!
//! This is `wasm_xz_conformance` with the `install()` call removed. Built for
//! `wasm32-wasip1` with `--no-default-features --features
//! std,crc-host,crypto-host,xz`, it goes straight at a checksum with an empty
//! hook registry, which must **panic** with the message
//! [`lzma_turbo::hooks`] documents - not fall back to an in-guest
//! implementation, which would silently return correct bytes at the speed the
//! embedding exists to avoid, and not return a wrong answer.
//!
//! It declares no imports at all, so it needs no host: the driver in
//! `tools/wasm-conformance/tests/wasm_host_conformance.rs` instantiates it
//! under plain WASI, runs it, and requires both a non-zero exit (a panic under
//! `panic = "abort"` traps) and the documented text on stderr.
//!
//! Run it through the driver:
//!   cargo test -p wasm-conformance
//!
//! Natively it prints the checksum and exits 0, because the delegating
//! features are inert off wasm - which is itself the claim the native lane
//! makes, and the driver asserts that difference explicitly.

use std::io::Write;

fn main() {
    // Deliberately NO `install_host_hash_hooks` call. On wasm this is the
    // missing wiring the panic names; natively it is simply irrelevant.
    let data: Vec<u8> = (0u32..4096).map(|i| (i * 37 + 11) as u8).collect();

    let mut stderr = std::io::stderr();
    let _ = writeln!(
        stderr,
        "wasm_missing_hooks: hooks installed = {}",
        lzma_turbo::hooks::host_hash_hooks_installed()
    );
    let _ = stderr.flush();

    // On a `crc-host` wasm build this call reaches the empty registry and
    // panics. Anywhere else it is an ordinary CRC-32.
    let crc = lzma_turbo::crc::crc32(&data);

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "crc32={crc:#010x} (no host was needed)");
    let _ = stdout.flush();
}
