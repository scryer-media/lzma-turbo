//! `Lzma2ParallelDecoder::decode_at`: reading every run at once must give the
//! bytes that `decode` gives over the same stream, and fail where it fails.
#![cfg(feature = "std")]

mod common;

use std::io::Write;

use lzma_turbo::{Lzma2MtOptions, Lzma2ParallelDecoder};

use common::multi_run;

const THREADS: &[usize] = &[1, 2, 3, 8, 16];

fn dec(prop: u8, threads: usize, memory_limit: u64) -> Lzma2ParallelDecoder {
    Lzma2ParallelDecoder::new(
        prop,
        &Lzma2MtOptions {
            threads,
            memory_limit,
        },
    )
    .expect("props")
}

/// What `decode` gives: the bytes written before it stopped, and the error
/// kind if it stopped on one.
fn by_stream(d: &Lzma2ParallelDecoder, packed: &[u8]) -> (Vec<u8>, Option<std::io::ErrorKind>) {
    let mut out = Vec::new();
    let r = d.decode(packed, &mut out);
    (out, r.err().map(|e| e.kind()))
}

fn by_offset(
    d: &Lzma2ParallelDecoder,
    src: &[u8],
    offset: u64,
    len: u64,
) -> (Vec<u8>, Option<std::io::ErrorKind>) {
    let mut out = Vec::new();
    let r = d.decode_at(src, offset, len, &mut out);
    if let Ok(n) = r {
        assert_eq!(n, out.len() as u64, "the count is what was written");
    }
    (out, r.err().map(|e| e.kind()))
}

/// `decode_at`'s output against `decode`'s: equal on success; on failure
/// `decode` may have dropped a whole block of runs that `decode_at` already
/// wrote, so its output is a prefix of `decode_at`'s.
fn same_output(got: &[u8], want: &(Vec<u8>, Option<std::io::ErrorKind>), ctx: &str) {
    if want.1.is_none() {
        assert_eq!(got, want.0.as_slice(), "{ctx}");
    } else {
        assert!(
            got.starts_with(&want.0),
            "{ctx}: decode wrote {} bytes decode_at did not repeat",
            want.0.len()
        );
    }
}

fn stream() -> (u8, Vec<u8>, Vec<u8>) {
    multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 4)
}

#[test]
fn decode_at_matches_the_plain_text_at_every_width() {
    let (prop, packed, plain) = stream();
    for &t in THREADS {
        let (got, err) = by_offset(&dec(prop, t, u64::MAX), &packed, 0, packed.len() as u64);
        assert_eq!(err, None, "threads={t}");
        assert_eq!(got, plain, "threads={t}");
    }
}

#[test]
fn decode_at_reads_from_inside_a_larger_source() {
    let (prop, packed, plain) = stream();
    let mut src = vec![0x5Au8; 37];
    src.extend_from_slice(&packed);
    src.extend_from_slice(b"bytes after the end marker are never read");
    for len in [packed.len() as u64, u64::MAX] {
        for &t in THREADS {
            let (got, err) = by_offset(&dec(prop, t, u64::MAX), &src, 37, len);
            assert_eq!(err, None, "threads={t} len={len}");
            assert_eq!(got, plain, "threads={t} len={len}");
        }
    }
}

#[test]
fn decode_at_reads_a_file() {
    let (prop, packed, plain) = stream();
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mt_at_stream.lzma2");
    {
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"head").expect("write");
        f.write_all(&packed).expect("write");
    }
    let file = std::fs::File::open(&path).expect("open");
    for &t in THREADS {
        let mut out = Vec::new();
        let n = dec(prop, t, u64::MAX)
            .decode_at(&file, 4, u64::MAX, &mut out)
            .unwrap_or_else(|e| panic!("threads={t}: {e}"));
        assert_eq!(n, plain.len() as u64);
        assert_eq!(out, plain, "threads={t}");
    }
    drop(file);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn decode_at_handles_a_single_run_stream() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz"], 1);
    for &t in THREADS {
        let (got, err) = by_offset(&dec(prop, t, u64::MAX), &packed, 0, u64::MAX);
        assert_eq!(err, None, "threads={t}");
        assert_eq!(got, plain, "threads={t}");
    }
}

#[test]
fn decode_at_under_a_memory_limit_still_decodes() {
    let (prop, packed, plain) = stream();
    for limit in [1u64, 1 << 20, 64 << 20] {
        let (got, err) = by_offset(&dec(prop, 8, limit), &packed, 0, u64::MAX);
        assert_eq!(err, None, "limit={limit}");
        assert_eq!(got, plain, "limit={limit}");
    }
}

#[test]
fn decode_at_fails_as_decode_does_on_damage() {
    let (prop, packed, _) = stream();
    // Flip bytes across the stream, in memory, one damage case at a time.
    let step = packed.len() / 23;
    for k in 1..23 {
        let mut bad = packed.clone();
        bad[k * step] ^= 0x55;
        for &t in &[2usize, 8] {
            let d = dec(prop, t, u64::MAX);
            let want = by_stream(&d, &bad);
            let got = by_offset(&d, &bad, 0, bad.len() as u64);
            assert_eq!(got.1, want.1, "threads={t} at={}", k * step);
            same_output(&got.0, &want, &format!("threads={t} at={}", k * step));
        }
    }
}

#[test]
fn decode_at_fails_as_decode_does_on_truncation() {
    let (prop, packed, _) = stream();
    for cut in [
        1usize,
        100,
        packed.len() / 3,
        packed.len() / 2,
        packed.len() - 1,
    ] {
        let short = &packed[..cut];
        for &t in &[2usize, 8] {
            let d = dec(prop, t, u64::MAX);
            let want = by_stream(&d, short);
            let got = by_offset(&d, short, 0, u64::MAX);
            assert_eq!(got.1, want.1, "threads={t} cut={cut}");
            same_output(&got.0, &want, &format!("threads={t} cut={cut}"));
        }
    }
}

#[test]
fn decode_at_keeps_the_runs_before_a_damaged_one() {
    // Found by the `decode_lzma2_at` fuzz target: a one-byte stored run, then
    // an LZMA run that cannot decode. `decode` holds both in one block and
    // drops it on the error; `decode_at` writes the first run before failing.
    let packed = [
        0x01, 0x00, 0x00, 0xFF, 0xEF, 0xFF, 0x01, 0x04, 0x01, 0x00, 0xBF, 0xFB,
    ];
    let d = dec(12, 5, 1 << 26);
    let want = by_stream(&d, &packed);
    let got = by_offset(&d, &packed, 0, u64::MAX);
    assert!(want.1.is_some() && got.1 == want.1, "{got:?} vs {want:?}");
    assert_eq!(got.0, [0xFF]);
    same_output(&got.0, &want, "fuzz case");
}

/// A source that remembers which threads read from it.
struct Witness<'a> {
    data: &'a [u8],
    readers: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

impl lzma_turbo::ReadAt for Witness<'_> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        let name = std::thread::current().name().unwrap_or("").to_owned();
        self.readers.lock().expect("lock").insert(name);
        self.data.read_at(buf, offset)
    }
}

#[test]
fn decode_at_reads_runs_from_its_workers() {
    let (prop, packed, plain) = stream();
    let w = Witness {
        data: &packed,
        readers: std::sync::Mutex::new(std::collections::BTreeSet::new()),
    };
    let mut out = Vec::new();
    dec(prop, 4, u64::MAX)
        .decode_at(&w, 0, u64::MAX, &mut out)
        .expect("decodes");
    assert_eq!(out, plain);
    let readers = w.readers.into_inner().expect("lock");
    let workers = readers
        .iter()
        .filter(|n| n.starts_with("lzma2-at-"))
        .count();
    assert!(workers >= 2, "runs were read by the workers: {readers:?}");
}

#[test]
fn decode_at_handles_a_stream_of_many_tiny_runs() {
    // Twenty thousand one-byte runs: every run is held only while in flight.
    let runs: Vec<common::Run> = (0..20_000u32)
        .map(|i| common::copy_run(&[(i % 251) as u8]))
        .collect();
    let (packed, plain) = common::join_runs(&runs);
    let prop = 0u8;
    for &t in &[2usize, 16] {
        let d = dec(prop, t, u64::MAX);
        let (got, err) = by_offset(&d, &packed, 0, u64::MAX);
        assert_eq!(err, None, "threads={t}");
        assert_eq!(got, plain, "threads={t}");
        assert_eq!(by_stream(&d, &packed).0, plain, "threads={t}");
    }
}
