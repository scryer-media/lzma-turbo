//! A report of what this crate decodes and encodes, for comparing a
//! WebAssembly build with the native one.
//!
//! It reads every `.xz` and `.lzma` stream in a fixture directory, in name
//! order, and prints one line each: the decoded length and its SHA-256, or
//! the refusal a caller would see. With the `enc` feature it then encodes
//! each `src_*.bin` to `.xz` and to `.lzma` and prints the SHA-256 of the
//! encoded bytes, which the encoder guarantees are the same on every
//! platform. Nothing is delegated to a host: the checks are computed in the
//! module, so the program runs under a plain `wasmtime` or Node.
//!
//! CI builds it natively and for `wasm32-wasip1`, `wasm32-wasip2` and
//! `wasm32-unknown-emscripten`, and requires every report to equal the
//! native one byte for byte.
//!
//! Build (the feature set every lane uses):
//!   cargo build --release -p lzma-turbo --no-default-features \
//!     --features std,asm,crc,native-crypto,xz,enc \
//!     --target wasm32-wasip2 --example wasm_portable_check
//!
//! Run:
//!   wasmtime run --dir tests/data::/fixtures wasm_portable_check.wasm /fixtures

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use lzma_turbo::crypto::Sha256;
use lzma_turbo::{LzmaReader, XzReader};

fn fingerprint(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hash = Sha256::new();
    for chunk in bytes.chunks(4096) {
        hash.update(chunk);
    }
    let mut text = String::with_capacity(64);
    for byte in hash.finalize() {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The fixture names with this extension, sorted: directory order differs
/// between hosts and the report must not.
fn names(root: &Path, extension: &str) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if Path::new(name).extension().is_some_and(|e| e == extension) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn decoded(result: std::io::Result<Vec<u8>>) -> String {
    match result {
        Ok(bytes) => format!("{} bytes sha256 {}", bytes.len(), fingerprint(&bytes)),
        Err(error) => format!("refused: {error}"),
    }
}

fn decode_xz(stream: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    XzReader::new(stream).read_to_end(&mut out)?;
    Ok(out)
}

fn decode_lzma(stream: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    LzmaReader::new(stream)?.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(feature = "enc")]
fn encode_rows(root: &Path, lines: &mut Vec<String>) -> std::io::Result<()> {
    use lzma_turbo::xz::CheckType;
    use lzma_turbo::{LzmaEncProps, encode_lzma_alone, encode_xz};

    for name in names(root, "bin")? {
        let source = std::fs::read(root.join(&name))?;
        for level in [1, 6] {
            let props = LzmaEncProps::new()
                .with_level(level)
                .with_dict_size(1 << 20);
            let xz = match encode_xz(&source, &props, CheckType::Crc64, 1 << 16) {
                Ok(bytes) => format!("{} bytes sha256 {}", bytes.len(), fingerprint(&bytes)),
                Err(error) => format!("refused: {error}"),
            };
            lines.push(format!("encode | {name} | level {level} | xz | {xz}"));
            let lzma = match encode_lzma_alone(&source, &props) {
                Ok(bytes) => format!("{} bytes sha256 {}", bytes.len(), fingerprint(&bytes)),
                Err(error) => format!("refused: {error}"),
            };
            lines.push(format!("encode | {name} | level {level} | lzma | {lzma}"));
        }
    }
    Ok(())
}

fn report(root: &Path) -> std::io::Result<Vec<String>> {
    let mut lines = Vec::new();
    for name in names(root, "xz")? {
        let stream = std::fs::read(root.join(&name))?;
        lines.push(format!("decode | {name} | {}", decoded(decode_xz(&stream))));
    }
    for name in names(root, "lzma")? {
        let stream = std::fs::read(root.join(&name))?;
        lines.push(format!(
            "decode | {name} | {}",
            decoded(decode_lzma(&stream))
        ));
    }
    #[cfg(feature = "enc")]
    encode_rows(root, &mut lines)?;
    Ok(lines)
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/fixtures"));
    let lines = match report(&root) {
        Ok(lines) => lines,
        Err(error) => {
            eprintln!("wasm_portable_check: {}: {error}", root.display());
            std::process::exit(2);
        }
    };
    let mut stdout = std::io::stdout();
    for line in &lines {
        let _ = writeln!(stdout, "{line}");
    }
    let _ = writeln!(stdout, "rows={}", lines.len());
    // Flush explicitly rather than rely on exit-time flushing: the report is
    // the result.
    let _ = stdout.flush();
}
