//! Real >4 GiB byte delivery with bounded fixture storage and fixed-size I/O.
#![cfg(all(feature = "enc", feature = "xz"))]

use lzma_turbo::{LzmaEncProps, XzReader, XzWriter};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::process::{Command, Stdio};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct LimitedFile {
    file: File,
    bytes: u64,
}
impl Write for LimitedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes + bytes.len() as u64 > 16 * 1024 * 1024 {
            return Err(std::io::Error::other(
                "generated compressed fixture exceeds 16 MiB cap",
            ));
        }
        let n = self.file.write(bytes)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn digest_reader(mut input: impl Read) -> (u64, Vec<u8>) {
    let mut bytes = [0; 64 * 1024];
    let mut total = 0u64;
    let mut digest = Sha256::new();
    loop {
        let n = input.read(&mut bytes).expect("stream decode");
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
        total += n as u64;
    }
    (total, digest.finalize().to_vec())
}

fn roundtrip(bytes: u64, block_size: u64) {
    // No entire-input Vec, sparse decompressed file, or read_to_end sink.
    // The pattern includes all byte values and is independent of read splits.
    let mut pattern = [0u8; 64 * 1024];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = i as u8;
    }
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "lzma-large-stream-{}-{}.xz", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    )));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&scratch.0)
        .unwrap();
    let props = LzmaEncProps::new().with_level(1).with_dict_size(64 * 1024);
    let mut writer = XzWriter::new(LimitedFile { file, bytes: 0 }, &props).unwrap();
    writer.set_block_size(block_size);
    let mut expected = Sha256::new();
    let mut remaining = bytes;
    while remaining > 0 {
        let n = remaining.min(pattern.len() as u64) as usize;
        writer.write_all(&pattern[..n]).unwrap();
        expected.update(&pattern[..n]);
        remaining -= n as u64;
    }
    let mut encoded = writer.finish().unwrap();
    encoded.file.rewind().unwrap();
    let expected = (bytes, expected.finalize().to_vec());
    assert_eq!(digest_reader(XzReader::new(encoded.file)), expected);
    // Explicit large-test runs require an independent decoder, never skip it.
    let mut oracle = Command::new("xz")
        .args(["-dc", "--threads=1"])
        .arg(&scratch.0)
        .stdout(Stdio::piped())
        .spawn()
        .expect("xz must be installed");
    let got = digest_reader(oracle.stdout.take().unwrap());
    assert!(
        oracle.wait().unwrap().success(),
        "xz rejected generated stream"
    );
    assert_eq!(got, expected);
}

#[test]
#[ignore = "streams >4 GiB through encoder and both decoders; <=16 MiB fixture storage"]
fn solid_xz_stream_crosses_four_gib_with_exact_digest() {
    roundtrip((1u64 << 32) + 65537, 0);
}

#[test]
#[ignore = "streams >4 GiB through encoder and both decoders; <=16 MiB fixture storage"]
fn multiblock_xz_stream_crosses_four_gib_with_exact_digest() {
    roundtrip((1u64 << 32) + 65537, 16 * 1024 * 1024);
}
