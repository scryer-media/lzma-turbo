//! The multi-threaded LZMA2 decoder: it must agree with the single-threaded
//! one, byte for byte, whatever the thread count and however the input is fed.
//!
//! The threaded decoders live behind the `std` feature, so this whole file
//! does with them.
#![cfg(feature = "std")]

mod common;

use lzma_turbo::{
    DrainStatus, Lzma2AdaptiveDecoder, Lzma2MtOptions, Lzma2ParallelDecoder, Lzma2RunScanner,
};

use common::multi_run;

const THREADS: &[usize] = &[1, 2, 3, 8, 16];

fn opts(threads: usize) -> Lzma2MtOptions {
    Lzma2MtOptions {
        threads,
        memory_limit: u64::MAX,
    }
}

fn decode_mt(dict_prop: u8, packed: &[u8], threads: usize) -> std::io::Result<Vec<u8>> {
    let dec = Lzma2ParallelDecoder::new(dict_prop, &opts(threads)).expect("props");
    let mut out = Vec::new();
    dec.decode(packed, &mut out)?;
    Ok(out)
}

#[test]
fn mt_matches_st_on_a_multi_run_stream() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 4);
    for &t in THREADS {
        let got = decode_mt(prop, &packed, t).unwrap_or_else(|e| panic!("threads={t}: {e}"));
        assert_eq!(got, plain, "threads={t}");
    }
}

#[test]
fn mt_matches_st_on_a_single_run_stream() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz"], 1);
    for &t in THREADS {
        let got = decode_mt(prop, &packed, t).unwrap_or_else(|e| panic!("threads={t}: {e}"));
        assert_eq!(got, plain, "threads={t}");
    }
}

// ---------------------------------------------------------------------------
// Differential against the single-threaded decoder, in odd-sized pieces
// ---------------------------------------------------------------------------

/// A `Read` that hands out at most `chunk` bytes at a time, so the threaded
/// decoder's own read loop has to deal with short reads.
struct ChunkReader<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl std::io::Read for ChunkReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.chunk.min(buf.len()).min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

fn decode_mt_chunked(
    dict_prop: u8,
    packed: &[u8],
    threads: usize,
    chunk: usize,
) -> std::io::Result<Vec<u8>> {
    let dec = Lzma2ParallelDecoder::new(dict_prop, &opts(threads)).expect("props");
    let mut out = Vec::new();
    dec.decode(
        ChunkReader {
            data: packed,
            chunk,
        },
        &mut out,
    )?;
    Ok(out)
}

#[test]
fn odd_input_chunk_sizes_change_nothing() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 3);
    for &t in THREADS {
        for chunk in [1usize, 3, 17, 251, 4099, 65_537] {
            let got = decode_mt_chunked(prop, &packed, t, chunk)
                .unwrap_or_else(|e| panic!("threads={t} chunk={chunk}: {e}"));
            assert_eq!(got, plain, "threads={t} chunk={chunk}");
        }
    }
}

// ---------------------------------------------------------------------------
// Truncation and corruption
// ---------------------------------------------------------------------------

/// The run boundaries of a stream, as offsets into it.
fn run_boundaries(packed: &[u8]) -> Vec<(u64, u64)> {
    let mut s = Lzma2RunScanner::new();
    s.feed(packed).expect("scan");
    let mut v = Vec::new();
    while let Some(r) = s.next_run() {
        v.push((r.in_offset, r.packed_len));
    }
    v
}

/// Decodes single-threaded, the way the rest of the crate does, for comparison.
fn decode_st(dict_prop: u8, packed: &[u8]) -> Result<Vec<u8>, ()> {
    common::decode_lzma2(dict_prop, packed, 1 << 16, 1 << 16)
        .ok()
        .and_then(|d| {
            if d.status == lzma_turbo::Status::FinishedWithMark {
                Some(d.bytes)
            } else {
                None
            }
        })
        .ok_or(())
}

#[test]
fn truncation_anywhere_is_an_error_and_never_a_hang() {
    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 2);
    let bounds = run_boundaries(&packed);
    assert_eq!(bounds.len(), 6);

    let mut cuts: Vec<usize> = Vec::new();
    for (off, len) in &bounds {
        // Exactly at the boundary, and three places inside the run.
        cuts.push(*off as usize);
        for d in [1u64, len / 3, len - 1] {
            cuts.push((*off + d) as usize);
        }
    }
    // And one byte short of the end marker.
    cuts.push(packed.len() - 1);
    cuts.retain(|c| *c > 0 && *c < packed.len());
    cuts.sort_unstable();
    cuts.dedup();

    for &cut in &cuts {
        for &t in &[1usize, 2, 8] {
            let r = decode_mt_chunked(prop, &packed[..cut], t, 4096);
            assert!(
                r.is_err(),
                "truncated at {cut}, threads={t}, decoded anyway"
            );
        }
    }
}

#[test]
fn corruption_in_any_run_agrees_with_the_single_threaded_decoder() {
    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz", "rand.p1.xz"], 2);
    let bounds = run_boundaries(&packed);

    for (i, (off, len)) in bounds.iter().enumerate() {
        for d in [0u64, len / 2, len - 1] {
            let at = (off + d) as usize;
            let mut bad = packed.clone();
            bad[at] ^= 0xA5;

            let st = decode_st(prop, &bad);
            for &t in &[1usize, 2, 8] {
                let mt = decode_mt_chunked(prop, &bad, t, 8192);
                match (&st, &mt) {
                    // LZMA is not self-checking: a flipped bit sometimes still
                    // decodes. When it does, both paths must decode it the
                    // same way.
                    (Ok(a), Ok(b)) => assert_eq!(a, b, "run {i} byte {at} threads={t}"),
                    (Err(()), Err(_)) => {}
                    (Ok(_), Err(e)) => {
                        panic!("run {i} byte {at} threads={t}: ST decoded, MT said {e}")
                    }
                    (Err(()), Ok(_)) => {
                        panic!("run {i} byte {at} threads={t}: MT decoded what ST rejected")
                    }
                }
            }
        }
    }
}

#[test]
fn a_corrupt_run_is_reported_with_its_index_and_offset() {
    use lzma_turbo::Error;

    let (prop, packed, _) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 3);
    let bounds = run_boundaries(&packed);

    // Break the middle of the fourth run's compressed payload, where the range
    // coder cannot help but notice.
    let (off, len) = bounds[3];
    let mut bad = packed.clone();
    for i in 0..16 {
        bad[(off + len / 2) as usize + i] ^= 0x5A;
    }

    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(4)).expect("props");
    dec.feed(&bad).expect("feed");
    dec.end_of_input();
    let mut delivered = 0u64;
    let err = loop {
        match dec.drain(|_, b| delivered += b.len() as u64) {
            Ok(DrainStatus::Finished) => panic!("corrupt stream decoded"),
            Ok(_) => {}
            Err(e) => break e,
        }
    };
    match err {
        Error::CorruptRun {
            index, out_offset, ..
        } => {
            assert_eq!(index, 3, "wrong run blamed");
            // Everything before the bad run was delivered and is still good.
            assert_eq!(delivered, out_offset);
        }
        other => panic!("expected a located error, got {other}"),
    }
}

#[test]
fn no_worker_outlives_the_decode() {
    let (prop, packed, plain) = multi_run(&["text.p1.xz", "mixed.p1.xz"], 4);

    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(8)).expect("props");
    dec.feed(&packed).expect("feed");
    dec.end_of_input();
    let mut out = Vec::new();
    while dec.drain(|_, b| out.extend_from_slice(b)).expect("drain") != DrainStatus::Finished {}
    assert_eq!(out, plain);
    assert!(dec.spawned_threads() > 0, "nothing ran on a worker");
    assert_eq!(dec.live_threads(), dec.spawned_threads());

    dec.cancel();
    assert_eq!(dec.live_threads(), 0, "a worker outlived cancel");

    // And dropping a decoder mid-decode joins its workers too.
    let mut dec = Lzma2AdaptiveDecoder::new(prop, &opts(8)).expect("props");
    dec.feed(&packed).expect("feed");
    let _ = dec.drain(|_, _| {});
    drop(dec);
}
