//! Real >4 GiB byte delivery with bounded fixture storage and fixed-size I/O,
//! through the sequential reader, the parallel reader, the adaptive decoder
//! and the system `xz`. Ignored by default; the weekly `large-streams` CI job
//! runs them.
#![cfg(all(feature = "enc", feature = "xz"))]

use lzma_turbo::{
    DrainStatus, LzmaEncProps, XzAdaptiveDecoder, XzOptions, XzParallelReader, XzReader, XzWriter,
};
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

/// The adaptive decoder, fed from `input` in fixed pieces; its output must
/// arrive in order.
fn digest_adaptive(mut input: impl Read) -> (u64, Vec<u8>) {
    let mut dec = XzAdaptiveDecoder::new(XzOptions::default().with_threads(2));
    let mut piece = vec![0u8; 1 << 20];
    let (mut filled, mut used) = (0usize, 0usize);
    let mut ended = false;
    let mut total = 0u64;
    let mut digest = Sha256::new();
    loop {
        if used == filled && !ended {
            filled = input.read(&mut piece).expect("fixture read");
            used = 0;
            if filled == 0 {
                ended = true;
                dec.end_of_input();
            }
        }
        if used < filled {
            used += dec.feed(&piece[used..filled]).expect("adaptive feed");
        }
        let status = dec
            .drain(|offset, bytes| {
                assert_eq!(offset, total, "adaptive output out of order");
                digest.update(bytes);
                total += bytes.len() as u64;
            })
            .expect("adaptive decode");
        match status {
            DrainStatus::Finished => break,
            DrainStatus::NeedsMoreInput => assert!(!ended, "wanted input after the end"),
            DrainStatus::Progress => {}
        }
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
    assert_eq!(digest_reader(XzReader::new(&encoded.file)), expected);
    encoded.file.rewind().unwrap();
    let parallel =
        XzParallelReader::with_options(&encoded.file, XzOptions::default().with_threads(2))
            .expect("parallel reader");
    assert_eq!(digest_reader(parallel), expected, "parallel reader");
    encoded.file.rewind().unwrap();
    assert_eq!(digest_adaptive(&encoded.file), expected, "adaptive decoder");
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
#[ignore = "streams >4 GiB through the encoder, every decoder and xz; <=16 MiB fixture storage"]
fn solid_xz_stream_crosses_four_gib_with_exact_digest() {
    roundtrip((1u64 << 32) + 65537, 0);
}

#[test]
#[ignore = "streams >4 GiB through the encoder, every decoder and xz; <=16 MiB fixture storage"]
fn multiblock_xz_stream_crosses_four_gib_with_exact_digest() {
    roundtrip((1u64 << 32) + 65537, 16 * 1024 * 1024);
}
