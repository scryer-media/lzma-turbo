//! Arbitrary bytes as an `.xz` file, through both decoders that accept a
//! non-seekable stream: the sequential reader and the adaptive one. Neither
//! may panic, neither may allocate without bound, and neither may loop
//! forever on input that has stopped arriving.
#![no_main]

use std::io::{Cursor, Read};

use libfuzzer_sys::fuzz_target;
use lzma_turbo::DrainStatus;
use lzma_turbo::xz::{XzAdaptiveDecoder, XzOptions, XzParallelReader, XzReader};

/// Small enough that a fuzz case cannot ask for a real allocation, large
/// enough that a `-0` stream still decodes.
const MEMORY_LIMIT: u64 = 8 << 20;
/// A bound on output, so a decompression bomb ends the case instead of the
/// machine.
const OUTPUT_LIMIT: u64 = 4 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    // The first byte picks the streaming shape, so stopping in the middle of
    // every field is fuzzed and not just the whole-buffer case.
    let chunk = match data[0] % 5 {
        0 => 1usize,
        1 => 3,
        2 => 64,
        3 => 4096,
        _ => usize::MAX,
    };
    let threads = usize::from(data[0] / 5 % 4) + 1;
    let data = &data[1..];

    let opts = XzOptions::default()
        .with_memory_limit(MEMORY_LIMIT)
        .with_max_unpack_bytes(Some(OUTPUT_LIMIT));

    // The sequential reader, read in `chunk`-sized pieces.
    let mut r = XzReader::with_options(data, opts.clone());
    let mut buf = vec![0u8; chunk.min(1 << 16).max(1)];
    let mut seq: Option<Vec<u8>> = Some(Vec::new());
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let out = seq.as_mut().expect("still ok");
                out.extend_from_slice(&buf[..n]);
                if out.len() as u64 > OUTPUT_LIMIT {
                    return;
                }
            }
            Err(_) => {
                seq = None;
                break;
            }
        }
    }

    // XZ Utils tests/ossfuzz/fuzz_decode_stream_mt.c exercises multithreaded
    // container decoding. Here the seekable parallel reader has separate
    // index/worker setup, in addition to the adaptive driver below.
    // Keep our normal checksum enforcement: never weaken production parsing
    // merely to reach farther into a fuzz input.
    if let Ok(mut parallel) =
        XzParallelReader::with_options(Cursor::new(data), opts.clone().with_threads(threads))
    {
        let mut out = Vec::new();
        if parallel
            .by_ref()
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut out)
            .is_ok()
        {
            assert!(out.len() as u64 <= OUTPUT_LIMIT);
            if let Some(seq) = &seq {
                assert_eq!(&out, seq);
            }
        }
    }

    // The adaptive decoder, fed in the same pieces. It must agree with the
    // sequential reader: same bytes, or a failure where it failed.
    let mut dec = XzAdaptiveDecoder::new(opts.with_threads(threads));
    let mut got: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut failed = false;
    loop {
        if pos < data.len() {
            let end = pos.saturating_add(chunk).min(data.len());
            match dec.feed(&data[pos..end]) {
                Ok(n) => pos += n,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
            if pos == data.len() {
                dec.end_of_input();
            }
        }
        let status = dec.drain(|off, bytes| {
            let Ok(off) = usize::try_from(off) else {
                return;
            };
            if off.saturating_add(bytes.len()) as u64 > OUTPUT_LIMIT {
                return;
            }
            if got.len() < off + bytes.len() {
                got.resize(off + bytes.len(), 0u8);
            }
            got[off..off + bytes.len()].copy_from_slice(bytes);
        });
        match status {
            Ok(lzma_turbo::DrainStatus::Finished) => break,
            Ok(DrainStatus::NeedsMoreInput) => {
                // The input is over and the decoder still wants some: that is
                // the bug this asserts against.
                assert!(pos < data.len(), "the decoder wanted input after the end");
            }
            Ok(DrainStatus::Progress) => {}
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    dec.cancel();

    if let Some(seq) = seq
        && !failed
        && (seq.len() as u64) < OUTPUT_LIMIT
    {
        assert_eq!(seq, got, "the adaptive decoder disagreed with the reader");
    }
});
